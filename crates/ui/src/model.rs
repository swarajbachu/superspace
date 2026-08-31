use std::path::PathBuf;
use superspace_core::{SearchCandidate, rank_candidates};

/// One secondary action available for a palette result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionItem {
    /// Stable action identifier.
    pub id: String,
    /// Visible title.
    pub title: String,
    /// Optional shortcut hint.
    pub shortcut: Option<String>,
}

/// Owned result content consumed by the platform-neutral palette state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaletteEntry {
    /// Stable result identifier.
    pub id: String,
    /// Primary visible title.
    pub title: String,
    /// Secondary context, such as a parent directory.
    pub subtitle: String,
    /// Semantic result category shown to the user.
    pub kind: PaletteEntryKind,
    /// Renderable icon path when the platform exposes one.
    pub icon: Option<PathBuf>,
    /// Search aliases.
    pub keywords: Vec<String>,
    /// Short preview description.
    pub preview: String,
    /// Usage count used as a ranking tie-breaker.
    pub frequency: u32,
    /// Whether the result is pinned.
    pub favorite: bool,
    /// Default and alternate actions.
    pub actions: Vec<ActionItem>,
}

/// Semantic category for a launcher result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaletteEntryKind {
    /// An installed desktop application.
    Application,
    /// A file indexed on the local device.
    File,
    /// A launcher command.
    Command,
    /// An inline calculation or conversion.
    Calculation,
    /// A durable clipboard-history item.
    Clipboard,
}

impl PaletteEntryKind {
    /// Short label used as result metadata.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Application => "Application",
            Self::File => "File",
            Self::Command => "Command",
            Self::Calculation => "Copy result",
            Self::Clipboard => "Clipboard",
        }
    }
}

/// Active palette surface.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PaletteMode {
    /// Search result list.
    #[default]
    Results,
    /// Action list for the selected result.
    Actions,
}

/// Normalized key input supported consistently by GPUI backends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaletteKey {
    /// Insert typed text.
    Text(String),
    /// Delete the preceding Unicode scalar value.
    Backspace,
    /// Select the preceding row, wrapping at the boundary.
    Up,
    /// Select the next row, wrapping at the boundary.
    Down,
    /// Activate the selected result or action.
    Enter,
    /// Open the selected result's action menu.
    OpenActions,
    /// Close actions, clear the query, or dismiss the palette.
    Escape,
}

/// Side effect requested by the palette state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaletteEvent {
    /// No external action is needed.
    None,
    /// Invoke an action for an entry.
    Invoke {
        /// Result owning the action.
        entry_id: String,
        /// Action to invoke.
        action_id: String,
    },
    /// Hide the palette window.
    Dismiss,
}

/// Fully testable search, selection, preview, and action-menu state.
#[derive(Clone, Debug)]
pub struct PaletteModel {
    entries: Vec<PaletteEntry>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    mode: PaletteMode,
    selected_action: usize,
}

impl PaletteModel {
    /// Create a palette and rank its initial empty query.
    #[must_use]
    pub fn new(entries: Vec<PaletteEntry>) -> Self {
        let mut model = Self {
            entries,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            mode: PaletteMode::Results,
            selected_action: 0,
        };
        model.rerank();
        model
    }

    /// Current search text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Replace the complete search query, as used by native text input.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.rerank();
    }

    /// Current surface.
    #[must_use]
    pub const fn mode(&self) -> PaletteMode {
        self.mode
    }

    /// Ranked result entries.
    #[must_use]
    pub fn results(&self) -> impl ExactSizeIterator<Item = &PaletteEntry> {
        self.matches.iter().map(|&index| &self.entries[index])
    }

    /// Selected result, also used as the preview source.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&PaletteEntry> {
        self.matches
            .get(self.selected)
            .map(|&index| &self.entries[index])
    }

    /// Actions belonging to the selected result.
    #[must_use]
    pub fn actions(&self) -> &[ActionItem] {
        self.selected_entry().map_or(&[], |entry| &entry.actions)
    }

    /// Selected zero-based row on the active surface.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        match self.mode {
            PaletteMode::Results => self.selected,
            PaletteMode::Actions => self.selected_action,
        }
    }

    /// Select a visible row on the active surface.
    ///
    /// Returns `false` when `index` is outside the current result or action list.
    pub fn select(&mut self, index: usize) -> bool {
        let length = match self.mode {
            PaletteMode::Results => self.matches.len(),
            PaletteMode::Actions => self.actions().len(),
        };
        if index >= length {
            return false;
        }
        match self.mode {
            PaletteMode::Results => self.selected = index,
            PaletteMode::Actions => self.selected_action = index,
        }
        true
    }

    /// Select and invoke a visible row, as used by pointer input.
    pub fn invoke(&mut self, index: usize) -> PaletteEvent {
        if !self.select(index) {
            return PaletteEvent::None;
        }
        self.invoke_selected()
    }

    /// Replace all candidates while preserving the query and selection when possible.
    pub fn replace_entries(&mut self, entries: Vec<PaletteEntry>) {
        let selected_id = self.selected_entry().map(|entry| entry.id.clone());
        self.entries = entries;
        self.rerank();
        if let Some(selected_id) = selected_id
            && let Some(position) = self
                .matches
                .iter()
                .position(|&index| self.entries[index].id == selected_id)
        {
            self.selected = position;
        }
    }

    /// Apply persisted ranking metadata to an entry and rerank the current query.
    pub fn update_preference(
        &mut self,
        id: &str,
        alias: Option<&str>,
        favorite: bool,
        frequency: u32,
    ) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        entry
            .keywords
            .retain(|keyword| !keyword.starts_with("alias:"));
        if let Some(alias) = alias {
            entry.keywords.push(format!("alias:{alias}"));
            entry.keywords.push(alias.to_owned());
        }
        entry.favorite = favorite;
        entry.frequency = frequency;
        self.rerank();
        true
    }

    /// Apply one normalized keyboard event and return any requested side effect.
    pub fn key(&mut self, key: PaletteKey) -> PaletteEvent {
        match key {
            PaletteKey::Text(text) if self.mode == PaletteMode::Results => {
                self.query.push_str(&text);
                self.rerank();
            }
            PaletteKey::Backspace if self.mode == PaletteMode::Results => {
                self.query.pop();
                self.rerank();
            }
            PaletteKey::Up => self.move_selection(-1),
            PaletteKey::Down => self.move_selection(1),
            PaletteKey::OpenActions
                if self
                    .selected_entry()
                    .is_some_and(|entry| !entry.actions.is_empty()) =>
            {
                self.mode = PaletteMode::Actions;
                self.selected_action = 0;
            }
            PaletteKey::Enter => return self.invoke_selected(),
            PaletteKey::Escape if self.mode == PaletteMode::Actions => {
                self.mode = PaletteMode::Results;
                self.selected_action = 0;
            }
            PaletteKey::Escape if !self.query.is_empty() => {
                self.query.clear();
                self.rerank();
            }
            PaletteKey::Escape => return PaletteEvent::Dismiss,
            PaletteKey::Text(_) | PaletteKey::Backspace | PaletteKey::OpenActions => {}
        }
        PaletteEvent::None
    }

    fn rerank(&mut self) {
        let keyword_refs = self
            .entries
            .iter()
            .map(|entry| {
                entry
                    .keywords
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let ranked = rank_candidates(
            &self.query,
            self.entries
                .iter()
                .zip(&keyword_refs)
                .map(|(entry, keywords)| SearchCandidate {
                    id: &entry.id,
                    title: &entry.title,
                    keywords,
                    frequency: entry.frequency,
                    favorite: entry.favorite,
                }),
        );
        self.matches = ranked
            .into_iter()
            .filter_map(|matched| {
                self.entries
                    .iter()
                    .position(|entry| entry.id == matched.candidate.id)
            })
            .collect();
        self.selected = 0;
        self.mode = PaletteMode::Results;
        self.selected_action = 0;
    }

    fn move_selection(&mut self, delta: isize) {
        let length = match self.mode {
            PaletteMode::Results => self.matches.len(),
            PaletteMode::Actions => self.actions().len(),
        };
        if length == 0 {
            return;
        }
        let selection = match self.mode {
            PaletteMode::Results => &mut self.selected,
            PaletteMode::Actions => &mut self.selected_action,
        };
        *selection = selection.checked_add_signed(delta).unwrap_or(length - 1) % length;
    }

    fn invoke_selected(&self) -> PaletteEvent {
        let Some(entry) = self.selected_entry() else {
            return PaletteEvent::None;
        };
        let action = match self.mode {
            PaletteMode::Results => entry.actions.first(),
            PaletteMode::Actions => entry.actions.get(self.selected_action),
        };
        action.map_or(PaletteEvent::None, |action| PaletteEvent::Invoke {
            entry_id: entry.id.clone(),
            action_id: action.id.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<PaletteEntry> {
        [
            ("apps", "Applications", true),
            ("clipboard", "Clipboard History", false),
            ("calculator", "Calculator", false),
        ]
        .into_iter()
        .map(|(id, title, favorite)| PaletteEntry {
            id: id.into(),
            title: title.into(),
            subtitle: String::new(),
            kind: PaletteEntryKind::Command,
            icon: None,
            keywords: vec![id.into()],
            preview: format!("Preview {title}"),
            frequency: 0,
            favorite,
            actions: vec![ActionItem {
                id: "open".into(),
                title: "Open".into(),
                shortcut: Some("enter".into()),
            }],
        })
        .collect()
    }

    #[test]
    fn typing_ranks_previews_and_backspace_is_unicode_safe() {
        let mut model = PaletteModel::new(entries());
        model.key(PaletteKey::Text("clip😀".into()));
        assert!(model.results().next().is_none());
        model.key(PaletteKey::Backspace);
        assert_eq!(model.query(), "clip");
        assert_eq!(model.selected_entry().expect("selection").id, "clipboard");
        assert_eq!(
            model.selected_entry().expect("preview").preview,
            "Preview Clipboard History"
        );
    }

    #[test]
    fn navigation_wraps_and_action_menu_invokes_selected_action() {
        let mut model = PaletteModel::new(entries());
        model.key(PaletteKey::Up);
        assert_eq!(model.selected_index(), 2);
        model.key(PaletteKey::Down);
        assert_eq!(model.selected_index(), 0);
        model.key(PaletteKey::OpenActions);
        assert_eq!(model.mode(), PaletteMode::Actions);
        assert_eq!(
            model.key(PaletteKey::Enter),
            PaletteEvent::Invoke {
                entry_id: "apps".into(),
                action_id: "open".into()
            }
        );
        assert_eq!(model.key(PaletteKey::Escape), PaletteEvent::None);
        assert_eq!(model.mode(), PaletteMode::Results);
    }

    #[test]
    fn escape_clears_then_dismisses_and_replacement_preserves_selection() {
        let mut model = PaletteModel::new(entries());
        model.key(PaletteKey::Down);
        let selected = model.selected_entry().expect("selection").id.clone();
        let mut replacement = entries();
        replacement.reverse();
        model.replace_entries(replacement);
        assert_eq!(model.selected_entry().expect("preserved").id, selected);
        model.key(PaletteKey::Text("calc".into()));
        assert_eq!(model.key(PaletteKey::Escape), PaletteEvent::None);
        assert!(model.query().is_empty());
        assert_eq!(model.key(PaletteKey::Escape), PaletteEvent::Dismiss);
    }

    #[test]
    fn pointer_selection_obeys_active_surface_bounds() {
        let mut model = PaletteModel::new(entries());
        assert!(model.select(0));
        assert_eq!(model.selected_entry().unwrap().id, "apps");
        assert!(!model.select(99));

        assert_eq!(model.key(PaletteKey::OpenActions), PaletteEvent::None);
        assert_eq!(model.mode(), PaletteMode::Actions);
        assert_eq!(
            model.invoke(0),
            PaletteEvent::Invoke {
                entry_id: "apps".into(),
                action_id: "open".into(),
            }
        );
        assert_eq!(model.invoke(2), PaletteEvent::None);
    }

    #[test]
    fn persisted_preferences_affect_search_and_ordering() {
        let mut model = PaletteModel::new(entries());
        assert!(model.update_preference("clipboard", Some("pasteboard"), true, 7));
        assert_eq!(model.selected_entry().unwrap().id, "clipboard");
        for character in "pasteboard".chars() {
            model.key(PaletteKey::Text(character.to_string()));
        }
        assert_eq!(model.selected_entry().unwrap().id, "clipboard");
        assert!(!model.update_preference("missing", None, false, 0));
    }
}
