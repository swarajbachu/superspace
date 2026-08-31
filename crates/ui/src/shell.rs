use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, App, Context, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, Styled as _, Window, div, px,
};
use superspace_core::builtin_features;
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
        let applications =
            superspace_platform::discover_apps(&superspace_platform::default_app_roots())
                .unwrap_or_default()
                .into_iter()
                .map(|application| (format!("app:{}", application.id), application))
                .collect::<HashMap<_, _>>();
        entries.extend(applications.iter().map(|(id, application)| PaletteEntry {
            id: id.clone(),
            title: application.name.clone(),
            keywords: application.keywords.clone(),
            preview: format!("Launch {}", application.name),
            frequency: 0,
            favorite: false,
            actions: vec![ActionItem {
                id: "launch".into(),
                title: "Launch Application".into(),
                shortcut: Some("↵".into()),
            }],
        }));
        let status = format!("{} applications indexed", applications.len());
        Self {
            model: PaletteModel::new(entries),
            focus,
            status,
            applications,
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
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
        match self.model.key(key) {
            PaletteEvent::None => {}
            PaletteEvent::Invoke {
                entry_id,
                action_id,
            } => self.invoke(&entry_id, &action_id),
            PaletteEvent::Dismiss => window.remove_window(),
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn invoke(&mut self, entry_id: &str, action_id: &str) {
        if action_id == "launch" {
            self.status = self.applications.get(entry_id).map_or_else(
                || format!("Application disappeared: {entry_id}"),
                |application| match application.launch() {
                    Ok(process_id) => format!("Launched {} ({process_id})", application.name),
                    Err(error) => format!("Could not launch {}: {error}", application.name),
                },
            );
        } else {
            self.status = format!("Requested {action_id} for {entry_id}");
        }
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
        let theme = theme::get(cx);
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
                    .child("Open ↵    Actions ⌘K"),
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
