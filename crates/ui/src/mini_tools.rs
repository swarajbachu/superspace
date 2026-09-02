use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use superspace_emoji::search as search_emoji;
use superspace_productivity::{
    Item, ItemContent, ProductivityStore, resolve_command, resolve_quicklink, search_symbols,
};

use crate::{ActionItem, PaletteEntry, PaletteEntryKind};

pub(crate) struct MiniTools {
    store: ProductivityStore,
    items: HashMap<String, Item>,
    arguments: HashMap<String, String>,
}

impl MiniTools {
    pub(crate) fn open(root: &Path) -> Result<Self, String> {
        Ok(Self {
            store: ProductivityStore::open(root.join("productivity.sqlite"))
                .map_err(|error| error.to_string())?,
            items: HashMap::new(),
            arguments: HashMap::new(),
        })
    }

    pub(crate) fn entries(&mut self, query: &str) -> Result<Vec<PaletteEntry>, String> {
        self.items.clear();
        self.arguments.clear();
        let mut entries = builtin_entries();
        for item in self
            .store
            .search(query, 100)
            .map_err(|error| error.to_string())?
        {
            let (id, mut entry) = item_entry(&item);
            if !query.is_empty() {
                entry.keywords.push(query.to_owned());
            }
            entries.push(entry);
            self.items.insert(id, item);
        }
        Ok(entries)
    }

    /// Resolve an exact leading keyword, preserving the remaining text as tool input.
    pub(crate) fn keyword_entry(&mut self, input: &str) -> Result<Option<PaletteEntry>, String> {
        let mut fields = input.trim().splitn(2, char::is_whitespace);
        let Some(keyword) = fields.next().filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let Some(item) = self
            .store
            .by_keyword(keyword)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        let arguments = fields.next().unwrap_or_default().trim().to_owned();
        let (id, mut entry) = item_entry(&item);
        entry.keywords.push(input.trim().to_owned());
        self.items.insert(id.clone(), item);
        self.arguments.insert(id, arguments);
        Ok(Some(entry))
    }

    pub(crate) fn emoji_entries(query: &str) -> Vec<PaletteEntry> {
        let mut entries = search_emoji(query)
            .into_iter()
            .map(|emoji| PaletteEntry {
                id: format!("emoji:{}", emoji.value),
                title: emoji.value.into(),
                subtitle: emoji.name.into(),
                kind: PaletteEntryKind::Emoji,
                icon: None,
                keywords: emoji
                    .keywords
                    .iter()
                    .copied()
                    .chain([emoji.name])
                    .map(str::to_owned)
                    .collect(),
                preview: format!("Copy {}", emoji.name),
                frequency: 0,
                favorite: false,
                actions: vec![ActionItem {
                    id: "copy-emoji".into(),
                    title: "Copy Emoji".into(),
                    shortcut: Some("↵".into()),
                }],
            })
            .collect::<Vec<_>>();
        entries.extend(search_symbols(query, 150).into_iter().map(|symbol| {
            PaletteEntry {
                id: format!("symbol:{}", symbol.value),
                title: symbol.value.into(),
                subtitle: symbol.name.into(),
                kind: PaletteEntryKind::Emoji,
                icon: None,
                keywords: symbol
                    .keywords
                    .iter()
                    .copied()
                    .chain([symbol.name])
                    .map(str::to_owned)
                    .collect(),
                preview: format!("Copy {}", symbol.name),
                frequency: 0,
                favorite: false,
                actions: vec![ActionItem {
                    id: "copy-symbol".into(),
                    title: "Copy Symbol".into(),
                    shortcut: Some("↵".into()),
                }],
            }
        }));
        entries
    }

    pub(crate) fn text(&self, id: &str) -> Option<String> {
        match &self.items.get(id)?.content {
            ItemContent::Snippet(value) | ItemContent::Note(value) => Some(value.clone()),
            _ => None,
        }
    }

    pub(crate) fn open_quicklink(&self, id: &str, query: &str) -> Result<(), String> {
        let ItemContent::Quicklink(template) = &self
            .items
            .get(id)
            .ok_or_else(|| "tool is no longer available".to_owned())?
            .content
        else {
            return Err("tool is not a quicklink".into());
        };
        let query = self.arguments.get(id).map_or(query, String::as_str);
        let url = resolve_quicklink(template, query);
        superspace_platform::open_path(&url)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub(crate) fn run_command(&self, id: &str, query: &str) -> Result<(), String> {
        let item = self
            .items
            .get(id)
            .ok_or_else(|| "tool is no longer available".to_owned())?;
        let query = self.arguments.get(id).map_or(query, String::as_str);
        let invocation = resolve_command(item, query).map_err(|error| error.to_string())?;
        Command::new(invocation.executable)
            .args(invocation.args)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn item_entry(item: &Item) -> (String, PaletteEntry) {
    let id = format!("productivity:{}", item.id);
    let (kind, subtitle, action, action_title) = match &item.content {
        ItemContent::Quicklink(_) => ("Quicklink", "Open URL", "open-quicklink", "Open"),
        ItemContent::Snippet(_) => ("Snippet", "Copy text", "copy-tool-item", "Copy"),
        ItemContent::Note(_) => ("Note", "Copy note", "copy-tool-item", "Copy"),
        ItemContent::Command { .. } => ("Command", "Run safely", "run-tool-command", "Run"),
    };
    let entry = PaletteEntry {
        id: id.clone(),
        title: item.title.clone(),
        subtitle: subtitle.into(),
        kind: PaletteEntryKind::Tool,
        icon: None,
        keywords: item
            .tags
            .iter()
            .cloned()
            .chain(item.keyword.iter().cloned())
            .chain([kind.to_owned()])
            .collect(),
        preview: kind.into(),
        frequency: u32::from(item.favorite) * 100,
        favorite: item.favorite,
        actions: vec![ActionItem {
            id: action.into(),
            title: action_title.into(),
            shortcut: Some("↵".into()),
        }],
    };
    (id, entry)
}

fn builtin_entries() -> Vec<PaletteEntry> {
    [
        (
            "tool:currency",
            "Currency Converter",
            "Fiat and crypto with live rates",
            "open-currency",
        ),
        (
            "tool:emoji",
            "Emoji & Symbols",
            "Find and copy emoji or symbols",
            "open-emoji",
        ),
        (
            "tool:uuid",
            "Generate UUID",
            "Copy a new UUID v4",
            "copy-uuid",
        ),
        (
            "tool:timestamp",
            "Unix Timestamp",
            "Copy the current Unix time",
            "copy-timestamp",
        ),
    ]
    .into_iter()
    .map(|(id, title, subtitle, action)| PaletteEntry {
        id: id.into(),
        title: title.into(),
        subtitle: subtitle.into(),
        kind: PaletteEntryKind::Tool,
        icon: None,
        keywords: vec![title.into(), subtitle.into(), "mini tool".into()],
        preview: subtitle.into(),
        frequency: 40,
        favorite: false,
        actions: vec![ActionItem {
            id: action.into(),
            title: title.into(),
            shortcut: Some("↵".into()),
        }],
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_search_produces_copyable_palette_entries() {
        let entries = MiniTools::emoji_entries("heart");
        assert!(!entries.is_empty());
        assert!(
            entries
                .iter()
                .all(|entry| entry.actions[0].id == "copy-emoji")
        );
        let mut model = crate::PaletteModel::new(entries);
        model.set_query("heart");
        assert!(model.results().next().is_some());

        let symbols = MiniTools::emoji_entries("command");
        assert!(symbols.iter().any(|entry| entry.title == "⌘"));
        assert!(
            symbols
                .iter()
                .any(|entry| entry.actions[0].id == "copy-symbol")
        );

        let complete_catalog = MiniTools::emoji_entries("");
        assert!(
            complete_catalog
                .iter()
                .filter(|entry| entry.actions[0].id == "copy-emoji")
                .count()
                > 150,
            "the picker must expose the complete emoji catalog"
        );
    }
}
