use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, App, Context, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use superspace_core::{LauncherPreferences, builtin_features};
use superspace_platform::AppDescriptor;

use crate::{
    ActionItem, PaletteEntry, PaletteEvent, PaletteKey, PaletteMode, PaletteModel, motion, theme,
};

/// Main command palette state.
pub struct Palette {
    model: PaletteModel,
    focus: FocusHandle,
    status: String,
    applications: HashMap<String, AppDescriptor>,
    base_entries: Vec<PaletteEntry>,
    file_index: Option<superspace_files::FileIndex>,
    files: HashMap<String, PathBuf>,
    theme_kind: theme::ThemeKind,
    preferences: LauncherPreferences,
    preferences_path: PathBuf,
}

impl Palette {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut entries = builtin_features()
            .iter()
            .map(|feature| PaletteEntry {
                id: feature.id.into(),
                title: feature.title.into(),
                keywords: vec![format!("{:?}", feature.area).to_ascii_lowercase()],
                preview: preview(feature.id).into(),
                frequency: 0,
                favorite: matches!(
                    feature.id,
                    "app-launcher" | "clipboard-history" | "nearby-share"
                ),
                actions: vec![
                    ActionItem {
                        id: "open".into(),
                        title: "Open".into(),
                        shortcut: Some("↵".into()),
                    },
                    ActionItem {
                        id: "favorite".into(),
                        title: "Toggle Favorite".into(),
                        shortcut: None,
                    },
                ],
            })
            .collect::<Vec<_>>();
        let preferences_path = data_root().join("launcher.json");
        let preferences = LauncherPreferences::load(&preferences_path).unwrap_or_default();
        for entry in &mut entries {
            apply_preference(entry, &preferences);
        }
        let applications =
            superspace_platform::discover_apps(&superspace_platform::default_app_roots())
                .unwrap_or_default()
                .into_iter()
                .map(|application| (format!("app:{}", application.id), application))
                .collect::<HashMap<_, _>>();
        entries.extend(applications.iter().map(|(id, application)| {
            let mut entry = PaletteEntry {
                id: id.clone(),
                title: application.name.clone(),
                keywords: application.keywords.clone(),
                preview: format!("Launch {}", application.name),
                frequency: 0,
                favorite: false,
                actions: vec![
                    ActionItem {
                        id: "launch".into(),
                        title: "Launch Application".into(),
                        shortcut: Some("↵".into()),
                    },
                    ActionItem {
                        id: "favorite".into(),
                        title: "Toggle Favorite".into(),
                        shortcut: None,
                    },
                ],
            };
            apply_preference(&mut entry, &preferences);
            entry
        }));
        let status = format!("{} applications indexed", applications.len());
        let base_entries = entries.clone();
        Self {
            model: PaletteModel::new(entries),
            focus,
            status,
            applications,
            base_entries,
            file_index: open_file_index(),
            files: HashMap::new(),
            theme_kind: theme::ThemeKind::default(),
            preferences,
            preferences_path,
        }
    }

    fn handle_event(&mut self, event: PaletteEvent, window: &mut Window) {
        match event {
            PaletteEvent::None => {}
            PaletteEvent::Invoke {
                entry_id,
                action_id,
            } => self.invoke(&entry_id, &action_id),
            PaletteEvent::Dismiss => window.remove_window(),
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key == "t"
            && keystroke.modifiers.shift
            && (keystroke.modifiers.platform || keystroke.modifiers.control)
        {
            self.theme_kind = self.theme_kind.next();
            self.status = format!("Theme: {}", self.theme_kind.name());
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let key = match keystroke.key.as_str() {
            "up" => Some(PaletteKey::Up),
            "down" => Some(PaletteKey::Down),
            "enter" => Some(PaletteKey::Enter),
            "escape" => Some(PaletteKey::Escape),
            "backspace" => Some(PaletteKey::Backspace),
            "k" if keystroke.modifiers.platform || keystroke.modifiers.control => {
                Some(PaletteKey::OpenActions)
            }
            _ if !keystroke.modifiers.platform && !keystroke.modifiers.control => {
                keystroke.key_char.clone().map(PaletteKey::Text)
            }
            _ => None,
        };
        let Some(key) = key else {
            return;
        };
        let query_changed = matches!(&key, PaletteKey::Text(_) | PaletteKey::Backspace);
        let palette_event = self.model.key(key);
        self.handle_event(palette_event, window);
        if query_changed {
            self.refresh_file_results();
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn invoke(&mut self, entry_id: &str, action_id: &str) {
        if action_id == "launch" {
            let application = self.applications.get(entry_id).cloned();
            self.status = application.map_or_else(
                || format!("Application disappeared: {entry_id}"),
                |application| match application.launch() {
                    Ok(process_id) => {
                        self.record_invocation(entry_id);
                        format!("Launched {} ({process_id})", application.name)
                    }
                    Err(error) => format!("Could not launch {}: {error}", application.name),
                },
            );
        } else if action_id == "open-file" {
            self.status = self.files.get(entry_id).map_or_else(
                || format!("File disappeared: {entry_id}"),
                |path| match superspace_platform::open_path(path) {
                    Ok(process_id) => format!("Opened {} ({process_id})", path.display()),
                    Err(error) => format!("Could not open {}: {error}", path.display()),
                },
            );
        } else if action_id == "nearby-share" {
            self.status = self.files.get(entry_id).map_or_else(
                || format!("File disappeared: {entry_id}"),
                |path| format!("Ready to share {} with a nearby device", path.display()),
            );
        } else if action_id == "favorite" {
            self.toggle_favorite(entry_id);
        } else {
            self.status = format!("Requested {action_id} for {entry_id}");
        }
    }

    fn toggle_favorite(&mut self, entry_id: &str) {
        match self.preferences.toggle_favorite(entry_id) {
            Ok(favorite) => {
                self.apply_saved_preference(entry_id);
                self.status = if favorite {
                    format!("Added {entry_id} to favorites")
                } else {
                    format!("Removed {entry_id} from favorites")
                };
            }
            Err(error) => self.status = format!("Could not update favorite: {error}"),
        }
    }

    fn record_invocation(&mut self, entry_id: &str) {
        if self.preferences.record_invocation(entry_id).is_ok() {
            self.apply_saved_preference(entry_id);
        }
    }

    fn apply_saved_preference(&mut self, entry_id: &str) {
        let Some(preference) = self.preferences.get(entry_id) else {
            return;
        };
        self.model.update_preference(
            entry_id,
            preference.alias.as_deref(),
            preference.favorite,
            preference.frequency,
        );
        if let Some(entry) = self
            .base_entries
            .iter_mut()
            .find(|entry| entry.id == entry_id)
        {
            apply_preference(entry, &self.preferences);
        }
        if let Err(error) = self.preferences.save(&self.preferences_path) {
            self.status = format!("Could not save launcher preferences: {error}");
        }
    }

    fn refresh_file_results(&mut self) {
        self.files.clear();
        let query = self.model.query().trim();
        let matches = if query.chars().count() >= 2 {
            self.file_index
                .as_ref()
                .and_then(|index| index.search(query, None, 30).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut entries = self.base_entries.clone();
        entries.extend(matches.into_iter().map(|matched| {
            let id = format!("file:{}", matched.path.display());
            self.files.insert(id.clone(), matched.path.clone());
            PaletteEntry {
                id,
                title: matched.name,
                keywords: Vec::new(),
                preview: format!("{} bytes · {}", matched.size, matched.path.display()),
                frequency: 0,
                favorite: false,
                actions: vec![
                    ActionItem {
                        id: "open-file".into(),
                        title: "Open File".into(),
                        shortcut: Some("↵".into()),
                    },
                    ActionItem {
                        id: "nearby-share".into(),
                        title: "Share with Nearby Device".into(),
                        shortcut: None,
                    },
                ],
            }
        }));
        self.model.replace_entries(entries);
    }
}

impl Focusable for Palette {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Palette {
    #[allow(
        clippy::too_many_lines,
        reason = "declarative palette layout is clearest as one tree"
    )]
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::for_kind(self.theme_kind);
        let matches = self.model.results().cloned().collect::<Vec<_>>();
        let selected = self.model.selected_entry().cloned();
        let selected_title = selected
            .as_ref()
            .map_or_else(|| "No results".into(), |entry| entry.title.clone());
        let selected_preview = selected.as_ref().map_or_else(
            || "Try another search".into(),
            |entry| entry.preview.clone(),
        );
        let rows = if self.model.mode() == PaletteMode::Actions {
            self.model
                .actions()
                .iter()
                .map(|action| (action.title.clone(), action.id.clone()))
                .collect::<Vec<_>>()
        } else {
            matches
                .iter()
                .map(|entry| (entry.title.clone(), entry.id.clone()))
                .collect::<Vec<_>>()
        };
        let selected_index = self.model.selected_index();

        div()
            .id("superspace-palette")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.text)
            .border_1()
            .border_color(theme.border)
            .rounded(px(18.0))
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .child(
                div()
                    .h(px(68.0))
                    .px(px(22.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_size(px(20.0))
                            .text_color(theme.accent)
                            .child("✦"),
                    )
                    .child(div().flex_1().text_size(px(20.0)).child(
                        if self.model.query().is_empty() {
                            "Search Superspace…".to_string()
                        } else {
                            self.model.query().to_string()
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme.muted)
                            .child("⌘ Space"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(390.0))
                            .p(px(10.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .children(rows.iter().take(10).enumerate().map(|(index, entry)| {
                                let selected = index == selected_index;
                                div()
                                    .id(("command-row", index))
                                    .h(px(42.0))
                                    .px(px(12.0))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .rounded(px(9.0))
                                    .when(selected, |row| row.bg(theme.selected))
                                    .hover(|row| row.bg(theme.selected))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let palette_event = this.model.invoke(index);
                                        this.handle_event(palette_event, window);
                                        cx.notify();
                                    }))
                                    .child(div().text_size(px(14.0)).child(entry.0.clone()))
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(theme.muted)
                                            .child(entry.1.clone()),
                                    )
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .p(px(24.0))
                            .border_l_1()
                            .border_color(theme.border)
                            .bg(theme.surface)
                            .flex()
                            .flex_col()
                            .gap(px(12.0))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(theme.accent)
                                    .child("SUPERSPACE"),
                            )
                            .child(div().text_size(px(22.0)).child(selected_title))
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.muted)
                                    .child(selected_preview),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(42.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_size(px(11.0))
                    .text_color(theme.muted)
                    .child(self.status.clone())
                    .child("Open ↵    Actions ⌘K    Theme ⇧⌘T"),
            )
            .with_animation(
                "palette-enter",
                motion::ENTER.animation(),
                |element, progress| {
                    element
                        .opacity(progress)
                        .relative()
                        .top(px(6.0 * (1.0 - progress)))
                },
            )
    }
}

fn preview(id: &str) -> &'static str {
    match id {
        "app-launcher" => "Discover and launch installed macOS and Linux applications.",
        "clipboard-history" => "Search, pin, and restore clipboard content across trusted devices.",
        "nearby-share" => "Send files and folders over encrypted local-network connections.",
        "calculator" => {
            "Calculate expressions and convert units, dates, currencies, and time zones."
        }
        "wasm-extensions" => {
            "Run capability-scoped WebAssembly extensions without ambient authority."
        }
        _ => "Open this Superspace command or press ⌘K / Ctrl+K for more actions.",
    }
}

fn open_file_index() -> Option<superspace_files::FileIndex> {
    superspace_files::FileIndex::open(data_root().join("files.sqlite")).ok()
}

fn apply_preference(entry: &mut PaletteEntry, preferences: &LauncherPreferences) {
    if let Some(preference) = preferences.get(&entry.id) {
        if let Some(alias) = &preference.alias {
            entry.keywords.push(format!("alias:{alias}"));
            entry.keywords.push(alias.clone());
        }
        entry.favorite = preference.favorite;
        entry.frequency = preference.frequency;
    }
}

fn data_root() -> PathBuf {
    std::env::var_os("SUPERSPACE_DATA_DIR").map_or_else(default_data_root, PathBuf::from)
}

fn default_data_root() -> PathBuf {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map_or_else(
            || PathBuf::from("Superspace"),
            |home| Path::new(&home).join("Library/Application Support/Superspace"),
        )
    } else {
        std::env::var_os("XDG_DATA_HOME").map_or_else(
            || {
                std::env::var_os("HOME").map_or_else(
                    || PathBuf::from(".local/share/superspace"),
                    |home| Path::new(&home).join(".local/share/superspace"),
                )
            },
            |root| Path::new(&root).join("superspace"),
        )
    }
}
