use serde::{Deserialize, Serialize};

/// Top-level areas exposed by the built-in feature catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureArea {
    /// Palette, application, file, and command discovery.
    Launcher,
    /// Clipboard history and nearby peer transfer.
    Sharing,
    /// Calculator, notes, snippets, links, and commands.
    Productivity,
    /// Window, calendar, system, and lifecycle integrations.
    Desktop,
    /// Chat providers and selected-text actions.
    Ai,
    /// Sandboxed third-party functionality.
    Extensions,
}

/// One stable built-in capability presented by Superspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinFeature {
    /// Stable identifier used by settings, commands, backups, and tests.
    pub id: &'static str,
    /// User-facing name.
    pub title: &'static str,
    /// Owning product area.
    pub area: FeatureArea,
}

/// The canonical built-in feature inventory.
#[must_use]
pub const fn builtin_features() -> &'static [BuiltinFeature] {
    &FEATURES
}

const FEATURES: [BuiltinFeature; 24] = [
    feature("app-launcher", "App Launcher", FeatureArea::Launcher),
    feature("file-search", "File Search", FeatureArea::Launcher),
    feature("hotkeys", "Global Hotkeys", FeatureArea::Launcher),
    feature(
        "clipboard-history",
        "Clipboard History",
        FeatureArea::Sharing,
    ),
    feature(
        "clipboard-sync",
        "Universal Clipboard",
        FeatureArea::Sharing,
    ),
    feature("nearby-share", "Nearby Share", FeatureArea::Sharing),
    feature("calculator", "Calculator", FeatureArea::Productivity),
    feature("currency", "Currency and Crypto", FeatureArea::Productivity),
    feature("quicklinks", "Quicklinks", FeatureArea::Productivity),
    feature("snippets", "Snippets", FeatureArea::Productivity),
    feature(
        "custom-commands",
        "Custom Commands",
        FeatureArea::Productivity,
    ),
    feature("notes", "Notes", FeatureArea::Productivity),
    feature("emoji", "Emoji Picker", FeatureArea::Productivity),
    feature(
        "window-management",
        "Window Management",
        FeatureArea::Desktop,
    ),
    feature("system-actions", "System Actions", FeatureArea::Desktop),
    feature("calendar", "Calendar and Meetings", FeatureArea::Desktop),
    feature("uninstall", "Application Uninstall", FeatureArea::Desktop),
    feature("backup", "Backup and Restore", FeatureArea::Desktop),
    feature("ai-chat", "AI Chat", FeatureArea::Ai),
    feature("quick-actions", "AI Quick Actions", FeatureArea::Ai),
    feature("ai-providers", "AI Providers", FeatureArea::Ai),
    feature(
        "wasm-extensions",
        "WebAssembly Extensions",
        FeatureArea::Extensions,
    ),
    feature(
        "extension-registry",
        "Extension Registry",
        FeatureArea::Extensions,
    ),
    feature(
        "extension-cli",
        "Extension Developer CLI",
        FeatureArea::Extensions,
    ),
];

const fn feature(id: &'static str, title: &'static str, area: FeatureArea) -> BuiltinFeature {
    BuiltinFeature { id, title, area }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn feature_ids_are_unique_and_stable_shaped() {
        let mut ids = HashSet::new();
        for feature in builtin_features() {
            assert!(ids.insert(feature.id), "duplicate id: {}", feature.id);
            assert!(
                feature
                    .id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            );
        }
    }
}
