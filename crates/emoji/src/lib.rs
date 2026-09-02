//! Complete, searchable Unicode emoji metadata.
//!
//! The catalog follows Unicode CLDR ordering and includes every skin-tone
//! sequence exposed by the bundled Unicode dataset. Call [`all`] for browsing
//! and [`search`] for ranked name, shortcode, and common-alias matching.

/// A high-level Unicode emoji category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Group {
    /// Faces, emotion, hearts, and gestures.
    SmileysAndEmotion,
    /// People, body parts, roles, and families.
    PeopleAndBody,
    /// Animals, plants, weather, and nature.
    AnimalsAndNature,
    /// Food, drink, meals, and ingredients.
    FoodAndDrink,
    /// Places, transport, time, and weather scenes.
    TravelAndPlaces,
    /// Sports, games, arts, and celebrations.
    Activities,
    /// Clothing, tools, devices, and household objects.
    Objects,
    /// Signs, controls, shapes, and other symbols.
    Symbols,
    /// Regional, subdivision, and special-purpose flags.
    Flags,
}

/// One emoji sequence and its searchable metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Emoji {
    /// Fully qualified Unicode grapheme.
    pub value: &'static str,
    /// Human-readable CLDR name.
    pub name: &'static str,
    /// High-level Unicode category.
    pub group: Group,
    /// GitHub shortcodes plus curated common aliases.
    pub keywords: Vec<&'static str>,
}

/// Return the complete emoji catalog in Unicode CLDR order.
///
/// Default and non-default skin-tone sequences are included. Values are
/// unique, and the returned order is stable for a fixed dependency version.
#[must_use]
pub fn all() -> Vec<Emoji> {
    let mut catalog = Vec::new();
    for emoji in emojis::iter() {
        if let Some(tones) = emoji.skin_tones() {
            catalog.extend(tones.map(to_record));
        } else {
            catalog.push(to_record(emoji));
        }
    }
    catalog
}

/// Search the complete catalog by value, name, shortcode, or common alias.
///
/// An empty query returns the full catalog in Unicode CLDR order. Non-empty
/// results are ordered by match quality while preserving catalog order for
/// equally strong matches.
#[must_use]
pub fn search(query: &str) -> Vec<Emoji> {
    let query = query.trim().to_ascii_lowercase();
    let mut matches = all()
        .into_iter()
        .enumerate()
        .filter_map(|(index, emoji)| match_score(&emoji, &query).map(|score| (score, index, emoji)))
        .collect::<Vec<_>>();
    matches.sort_by_key(|(score, index, _)| (*score, *index));
    matches.into_iter().map(|(_, _, emoji)| emoji).collect()
}

fn to_record(emoji: &'static emojis::Emoji) -> Emoji {
    Emoji {
        value: emoji.as_str(),
        name: emoji.name(),
        group: map_group(emoji.group()),
        keywords: emoji
            .shortcodes()
            .chain(extra_keywords(emoji.as_str()).iter().copied())
            .collect(),
    }
}

const fn map_group(group: emojis::Group) -> Group {
    match group {
        emojis::Group::SmileysAndEmotion => Group::SmileysAndEmotion,
        emojis::Group::PeopleAndBody => Group::PeopleAndBody,
        emojis::Group::AnimalsAndNature => Group::AnimalsAndNature,
        emojis::Group::FoodAndDrink => Group::FoodAndDrink,
        emojis::Group::TravelAndPlaces => Group::TravelAndPlaces,
        emojis::Group::Activities => Group::Activities,
        emojis::Group::Objects => Group::Objects,
        emojis::Group::Symbols => Group::Symbols,
        emojis::Group::Flags => Group::Flags,
    }
}

fn match_score(emoji: &Emoji, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let aliases = extra_keywords(emoji.value);
    if emoji.value == query || aliases.contains(&query) {
        Some(0)
    } else if emoji.keywords.contains(&query) {
        Some(1)
    } else if emoji.name == query {
        Some(2)
    } else if aliases.iter().any(|keyword| keyword.contains(query)) {
        Some(3)
    } else if emoji.keywords.iter().any(|keyword| keyword.contains(query)) {
        Some(4)
    } else if emoji.name.contains(query) {
        Some(5)
    } else {
        None
    }
}

fn extra_keywords(value: &str) -> &'static [&'static str] {
    match value {
        "😀" => &["smile", "happy"],
        "😂" => &["laugh", "lol"],
        "❤️" => &["love", "like"],
        "👍" => &["yes", "approve", "like"],
        "🎉" => &["celebrate", "tada"],
        "🚀" => &["launch", "ship"],
        "✅" => &["done", "success"],
        "🔥" => &["hot", "lit"],
        "👀" => &["look", "watch"],
        "🙏" => &["thanks", "please"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn catalog_is_complete_unique_and_searchable() {
        let catalog = all();
        assert!(catalog.len() > 3_000);
        assert_eq!(
            catalog.len(),
            catalog
                .iter()
                .map(|emoji| emoji.value)
                .collect::<HashSet<_>>()
                .len()
        );
        assert_eq!(search("ship")[0].value, "🚀");
        assert_eq!(search("like")[0].value, "❤️");
        assert_eq!(search("").len(), catalog.len());
    }
}
