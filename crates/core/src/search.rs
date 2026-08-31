use std::cmp::Ordering;

/// Searchable launcher content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchCandidate<'a> {
    /// Stable command identifier.
    pub id: &'a str,
    /// Primary display title.
    pub title: &'a str,
    /// Additional searchable words.
    pub keywords: &'a [&'a str],
    /// Usage score persisted by the launcher.
    pub frequency: u32,
    /// Whether the user explicitly pinned the item.
    pub favorite: bool,
}

/// A candidate and its deterministic relevance score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch<'a> {
    /// Matched candidate.
    pub candidate: SearchCandidate<'a>,
    /// Larger values rank first.
    pub score: i64,
}

/// Rank candidates using ordered-subsequence matching plus stable usage signals.
#[must_use]
pub fn rank_candidates<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = SearchCandidate<'a>>,
) -> Vec<SearchMatch<'a>> {
    let query = query.trim().to_lowercase();
    let mut matches = candidates
        .into_iter()
        .filter_map(|candidate| {
            let text_score = candidate_score(&query, candidate.title, candidate.keywords)?;
            let score = text_score
                + i64::from(candidate.frequency.min(10_000))
                + if candidate.favorite { 20_000 } else { 0 };
            Some(SearchMatch { candidate, score })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        right.score.cmp(&left.score).then_with(|| {
            left.candidate
                .title
                .to_lowercase()
                .cmp(&right.candidate.title.to_lowercase())
        })
    });
    matches
}

fn candidate_score(query: &str, title: &str, keywords: &[&str]) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let title = title.to_lowercase();
    if title == query {
        return Some(100_000);
    }
    if title.starts_with(query) {
        return Some(80_000 - i64::try_from(title.len()).unwrap_or(i64::MAX));
    }
    fuzzy_score(query, &title).or_else(|| {
        keywords
            .iter()
            .filter_map(|keyword| fuzzy_score(query, &keyword.to_lowercase()))
            .max()
            .map(|score| score - 5_000)
    })
}

fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    let mut query_chars = query.chars();
    let mut expected = query_chars.next()?;
    let mut first = None;
    let mut last = 0usize;
    let mut gaps = 0usize;
    let mut matched = 0usize;

    for (index, character) in text.chars().enumerate() {
        if character == expected {
            first.get_or_insert(index);
            if matched > 0 {
                gaps += index.saturating_sub(last + 1);
            }
            last = index;
            matched += 1;
            if let Some(next) = query_chars.next() {
                expected = next;
            } else {
                let start = i64::try_from(first.unwrap_or_default()).unwrap_or(i64::MAX);
                let gap_penalty = i64::try_from(gaps).unwrap_or(i64::MAX);
                return Some(50_000 - start * 100 - gap_penalty * 20);
            }
        }
    }
    None
}

#[allow(dead_code)]
fn ordering_is_total(left: &SearchMatch<'_>, right: &SearchMatch<'_>) -> Ordering {
    left.score
        .cmp(&right.score)
        .then_with(|| left.candidate.id.cmp(right.candidate.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: &[&str] = &[];

    fn item<'a>(
        id: &'a str,
        title: &'a str,
        frequency: u32,
        favorite: bool,
    ) -> SearchCandidate<'a> {
        SearchCandidate {
            id,
            title,
            keywords: NONE,
            frequency,
            favorite,
        }
    }

    #[test]
    fn exact_prefix_and_fuzzy_matches_rank_in_that_order() {
        let ranked = rank_candidates(
            "note",
            [
                item("fuzzy", "Nifty Output Text Editor", 0, false),
                item("prefix", "Notes", 0, false),
                item("exact", "note", 0, false),
            ],
        );
        assert_eq!(
            ranked
                .iter()
                .map(|entry| entry.candidate.id)
                .collect::<Vec<_>>(),
            ["exact", "prefix", "fuzzy"]
        );
    }

    #[test]
    fn favorites_lead_an_empty_query() {
        let ranked = rank_candidates(
            "",
            [
                item("used", "Used", 100, false),
                item("pin", "Pinned", 0, true),
            ],
        );
        assert_eq!(ranked[0].candidate.id, "pin");
    }

    #[test]
    fn unmatched_candidates_are_removed() {
        assert!(rank_candidates("xyz", [item("notes", "Notes", 0, false)]).is_empty());
    }
}
