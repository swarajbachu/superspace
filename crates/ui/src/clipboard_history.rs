use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use superspace_platform::{ClipboardBackend as _, NativeClipboard};
use superspace_storage::{
    BlobStore, ClipboardEntry, ClipboardKind, ClipboardQuery, ClipboardStore,
};

use crate::{ActionItem, PaletteEntry, PaletteEntryKind};

pub(crate) struct ClipboardHistory {
    store: ClipboardStore,
    blobs: BlobStore,
    entries: HashMap<String, ClipboardEntry>,
}

impl ClipboardHistory {
    pub(crate) fn open(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
        Ok(Self {
            store: ClipboardStore::open(root.join("clipboard.sqlite"))
                .map_err(|error| error.to_string())?,
            blobs: BlobStore::open(root.join("clipboard-blobs"))
                .map_err(|error| error.to_string())?,
            entries: HashMap::new(),
        })
    }

    pub(crate) fn search(&mut self, query: &str) -> Result<Vec<PaletteEntry>, String> {
        let entries = self
            .store
            .query(&ClipboardQuery {
                text: query,
                kind: None,
                include_sensitive: false,
                limit: 250,
            })
            .map_err(|error| error.to_string())?;
        self.entries.clear();
        Ok(entries
            .into_iter()
            .map(|entry| {
                let id = format!("clipboard:{}", entry.id);
                let (title, format) = presentation(&entry);
                let pinned = entry.pinned_at.is_some();
                let age = relative_age(now_ms(), entry.created_at);
                self.entries.insert(id.clone(), entry);
                PaletteEntry {
                    id,
                    title,
                    subtitle: format!("{format} · {age}"),
                    kind: PaletteEntryKind::Clipboard,
                    icon: None,
                    keywords: vec![format.to_owned(), "clipboard".into(), "history".into()],
                    preview: if pinned {
                        "Pinned clipboard item".into()
                    } else {
                        "Copy this item back to the clipboard".into()
                    },
                    frequency: 0,
                    favorite: pinned,
                    actions: vec![
                        ActionItem {
                            id: "restore-clipboard".into(),
                            title: "Copy to Clipboard".into(),
                            shortcut: Some("↵".into()),
                        },
                        ActionItem {
                            id: "toggle-clipboard-pin".into(),
                            title: if pinned { "Unpin".into() } else { "Pin".into() },
                            shortcut: None,
                        },
                        ActionItem {
                            id: "remove-clipboard".into(),
                            title: "Delete from History".into(),
                            shortcut: None,
                        },
                    ],
                }
            })
            .collect())
    }

    pub(crate) fn restore(&self, id: &str) -> Result<(), String> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| "clipboard item is no longer available".to_owned())?;
        let value = superspace_sync::restore_history_value(entry, &self.blobs)
            .map_err(|error| error.to_string())?;
        NativeClipboard::connect()
            .and_then(|mut clipboard| clipboard.write(&value))
            .map_err(|error| error.to_string())
    }

    pub(crate) fn toggle_pin(&self, id: &str) -> Result<bool, String> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| "clipboard item is no longer available".to_owned())?;
        let pinned = entry.pinned_at.is_none();
        self.store
            .set_pinned(entry.id, pinned.then_some(now_ms()))
            .map_err(|error| error.to_string())?;
        Ok(pinned)
    }

    pub(crate) fn remove(&self, id: &str) -> Result<(), String> {
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| "clipboard item is no longer available".to_owned())?;
        self.store
            .remove(entry.id)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn presentation(entry: &ClipboardEntry) -> (String, &'static str) {
    let format = match entry.kind {
        ClipboardKind::Text => "Text",
        ClipboardKind::Html => "Rich text",
        ClipboardKind::Rtf => "Rich text",
        ClipboardKind::Image => "Image",
        ClipboardKind::Files => "Files",
    };
    let title = entry.text.as_deref().map_or_else(
        || match entry.kind {
            ClipboardKind::Image => "Copied image".into(),
            ClipboardKind::Files => "Copied files".into(),
            ClipboardKind::Text | ClipboardKind::Html | ClipboardKind::Rtf => {
                "Clipboard item".into()
            }
        },
        compact_text,
    );
    (title, format)
}

fn compact_text(text: &str) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut characters = compact.chars();
    let title = characters.by_ref().take(100).collect::<String>();
    if characters.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

fn relative_age(now: i64, created_at: i64) -> String {
    let seconds = now.saturating_sub(created_at).max(0) / 1_000;
    match seconds {
        0..=59 => "Just now".into(),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::{compact_text, relative_age};

    #[test]
    fn clipboard_titles_are_single_line_bounded_and_aged() {
        assert_eq!(compact_text("hello\n  world"), "hello world");
        assert!(compact_text(&"x".repeat(101)).ends_with('…'));
        assert_eq!(relative_age(90_000, 0), "1m ago");
        assert_eq!(relative_age(7_200_000, 0), "2h ago");
    }
}
