use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, App, AppContext as _, BoxShadow, ClipboardItem, Context, Entity,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, point, px,
};
use superspace_calculator::{Calculator, CurrencyQuery, ResultValue};
use superspace_core::LauncherPreferences;
use superspace_platform::AppDescriptor;

use crate::{
    ActionItem, PaletteEntry, PaletteEntryKind, PaletteEvent, PaletteKey, PaletteMode,
    PaletteModel,
    clipboard_history::ClipboardHistory,
    currency::{self, Conversion},
    mini_tools::MiniTools,
    motion,
    search_input::{InputChanged, SearchInput},
    theme,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PaletteSurface {
    #[default]
    Launcher,
    Clipboard,
    Tools,
    Emoji,
    Currency,
}

/// Main command palette state.
pub struct Palette {
    model: PaletteModel,
    focus: FocusHandle,
    search_input: Entity<SearchInput>,
    results_scroll: ScrollHandle,
    surface: PaletteSurface,
    notice: Option<String>,
    application_count: usize,
    applications: HashMap<String, AppDescriptor>,
    base_entries: Vec<PaletteEntry>,
    file_index: Option<superspace_files::FileIndex>,
    files: HashMap<String, PathBuf>,
    calculations: HashMap<String, String>,
    clipboard: Option<ClipboardHistory>,
    mini_tools: Option<MiniTools>,
    currency_query: Option<CurrencyQuery>,
    currency_input: Option<String>,
    currency_result: Option<Result<Conversion, String>>,
    theme_kind: theme::ThemeKind,
    preferences: LauncherPreferences,
    preferences_path: PathBuf,
}

impl Palette {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(SearchInput::new);
        let results_scroll = ScrollHandle::new();
        let focus = search_input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);

        let preferences_path = data_root().join("launcher.json");
        let preferences = LauncherPreferences::load(&preferences_path).unwrap_or_default();
        let applications =
            superspace_platform::discover_apps(&superspace_platform::default_app_roots())
                .unwrap_or_default()
                .into_iter()
                .map(|application| (format!("app:{}", application.id), application))
                .collect::<HashMap<_, _>>();
        let mut entries = applications
            .iter()
            .map(|(id, application)| {
                let mut entry = PaletteEntry {
                    id: id.clone(),
                    title: application.name.clone(),
                    subtitle: String::new(),
                    kind: PaletteEntryKind::Application,
                    icon: application.icon.as_deref().and_then(prepare_icon),
                    keywords: application.keywords.clone(),
                    preview: format!("Open {}", application.name),
                    frequency: 0,
                    favorite: false,
                    actions: vec![
                        ActionItem {
                            id: "launch".into(),
                            title: "Open Application".into(),
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
            })
            .collect::<Vec<_>>();
        let application_count = applications.len();
        entries.push(builtin_entry(
            "builtin:clipboard",
            "Clipboard History",
            "Search, pin, restore, and remove copied items",
            "open-clipboard",
        ));
        entries.push(builtin_entry(
            "builtin:tools",
            "Mini Tools",
            "Currency, emoji, UUIDs, snippets, links, notes, and commands",
            "open-tools",
        ));
        let base_entries = entries.clone();
        let clipboard = ClipboardHistory::open(&data_root()).ok();
        let mini_tools = MiniTools::open(&data_root()).ok();

        cx.subscribe(&search_input, |palette, input, _: &InputChanged, cx| {
            palette.model.set_query(input.read(cx).text().to_owned());
            palette.notice = None;
            palette.refresh_results(cx);
            palette.results_scroll.scroll_to_item(0);
            cx.notify();
        })
        .detach();

        Self {
            model: PaletteModel::new(entries),
            focus,
            search_input,
            results_scroll,
            surface: PaletteSurface::Launcher,
            notice: None,
            application_count,
            applications,
            base_entries,
            file_index: open_file_index(),
            files: HashMap::new(),
            calculations: HashMap::new(),
            clipboard,
            mini_tools,
            currency_query: None,
            currency_input: None,
            currency_result: None,
            theme_kind: theme::ThemeKind::default(),
            preferences,
            preferences_path,
        }
    }

    fn handle_event(&mut self, event: PaletteEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaletteEvent::None => {}
            PaletteEvent::Invoke {
                entry_id,
                action_id,
            } => {
                if self.invoke(&entry_id, &action_id, cx) {
                    window.remove_window();
                }
            }
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
            self.notice = Some(format!("{} appearance", self.theme_kind.name()));
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let key = match keystroke.key.as_str() {
            "up" => Some(PaletteKey::Up),
            "down" => Some(PaletteKey::Down),
            "enter" => Some(PaletteKey::Enter),
            "escape" => Some(PaletteKey::Escape),
            "k" if keystroke.modifiers.platform || keystroke.modifiers.control => {
                Some(PaletteKey::OpenActions)
            }
            _ => None,
        };
        let Some(key) = key else {
            return;
        };
        if key == PaletteKey::Escape
            && self.model.mode() == PaletteMode::Results
            && !self.model.query().is_empty()
        {
            self.search_input.update(cx, SearchInput::clear);
            cx.stop_propagation();
            return;
        }
        if key == PaletteKey::Escape
            && self.model.mode() == PaletteMode::Results
            && self.surface != PaletteSurface::Launcher
        {
            self.enter_surface(PaletteSurface::Launcher, cx);
            cx.stop_propagation();
            return;
        }
        let palette_event = self.model.key(key);
        self.results_scroll
            .scroll_to_item(self.model.selected_index());
        self.handle_event(palette_event, window, cx);
        cx.stop_propagation();
        cx.notify();
    }

    /// Returns whether the palette should close after the action.
    #[allow(
        clippy::too_many_lines,
        reason = "central action routing keeps palette side effects explicit"
    )]
    fn invoke(&mut self, entry_id: &str, action_id: &str, cx: &mut Context<Self>) -> bool {
        if action_id == "open-clipboard" {
            self.enter_surface(PaletteSurface::Clipboard, cx);
            false
        } else if action_id == "open-tools" {
            self.enter_surface(PaletteSurface::Tools, cx);
            false
        } else if action_id == "open-emoji" {
            self.enter_surface(PaletteSurface::Emoji, cx);
            false
        } else if action_id == "open-currency" {
            self.enter_surface(PaletteSurface::Currency, cx);
            false
        } else if action_id == "restore-clipboard" {
            match self
                .clipboard
                .as_ref()
                .ok_or_else(|| "clipboard history is unavailable".to_owned())
                .and_then(|history| history.restore(entry_id))
            {
                Ok(()) => true,
                Err(error) => {
                    self.notice = Some(format!("Could not restore clipboard item: {error}"));
                    false
                }
            }
        } else if action_id == "toggle-clipboard-pin" {
            match self
                .clipboard
                .as_ref()
                .ok_or_else(|| "clipboard history is unavailable".to_owned())
                .and_then(|history| history.toggle_pin(entry_id))
            {
                Ok(pinned) => {
                    self.notice = Some(if pinned {
                        "Pinned clipboard item".into()
                    } else {
                        "Unpinned clipboard item".into()
                    });
                    self.refresh_results(cx);
                }
                Err(error) => {
                    self.notice = Some(format!("Could not update clipboard item: {error}"));
                }
            }
            false
        } else if action_id == "remove-clipboard" {
            match self
                .clipboard
                .as_ref()
                .ok_or_else(|| "clipboard history is unavailable".to_owned())
                .and_then(|history| history.remove(entry_id))
            {
                Ok(()) => {
                    self.notice = Some("Removed clipboard item".into());
                    self.refresh_results(cx);
                }
                Err(error) => {
                    self.notice = Some(format!("Could not remove clipboard item: {error}"));
                }
            }
            false
        } else if action_id == "launch" {
            let application = self.applications.get(entry_id).cloned();
            application.is_some_and(|application| match application.launch() {
                Ok(_) => {
                    self.record_invocation(entry_id);
                    true
                }
                Err(error) => {
                    self.notice = Some(format!("Could not open {}: {error}", application.name));
                    false
                }
            })
        } else if action_id == "open-file" {
            self.files.get(entry_id).cloned().is_some_and(|path| {
                match superspace_platform::open_path(&path) {
                    Ok(_) => true,
                    Err(error) => {
                        self.notice = Some(format!("Could not open {}: {error}", path.display()));
                        false
                    }
                }
            })
        } else if action_id == "copy-result" {
            if let Some(result) = self.calculations.get(entry_id) {
                cx.write_to_clipboard(ClipboardItem::new_string(result.clone()));
                self.notice = Some(format!("Copied {result}"));
            }
            false
        } else if action_id == "copy-currency" {
            if let Some(Ok(result)) = &self.currency_result {
                let value = result.display_value();
                cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
                self.notice = Some(format!("Copied {value} {}", result.query.to));
            }
            false
        } else if action_id == "copy-emoji" {
            if let Some(value) = entry_id.strip_prefix("emoji:") {
                cx.write_to_clipboard(ClipboardItem::new_string(value.to_owned()));
                self.notice = Some(format!("Copied {value}"));
            }
            false
        } else if action_id == "copy-uuid" {
            let value = uuid::Uuid::new_v4().to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(value));
            self.notice = Some("Copied a new UUID".into());
            false
        } else if action_id == "copy-timestamp" {
            let value = chrono::Utc::now().timestamp().to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
            self.notice = Some(format!("Copied {value}"));
            false
        } else if action_id == "copy-tool-item" {
            if let Some(value) = self
                .mini_tools
                .as_ref()
                .and_then(|tools| tools.text(entry_id))
            {
                cx.write_to_clipboard(ClipboardItem::new_string(value));
                self.notice = Some("Copied to clipboard".into());
            }
            false
        } else if action_id == "open-quicklink" {
            match self
                .mini_tools
                .as_ref()
                .ok_or_else(|| "mini tools are unavailable".to_owned())
                .and_then(|tools| tools.open_quicklink(entry_id, ""))
            {
                Ok(()) => true,
                Err(error) => {
                    self.notice = Some(format!("Could not open quicklink: {error}"));
                    false
                }
            }
        } else if action_id == "run-tool-command" {
            match self
                .mini_tools
                .as_ref()
                .ok_or_else(|| "mini tools are unavailable".to_owned())
                .and_then(|tools| tools.run_command(entry_id, ""))
            {
                Ok(()) => true,
                Err(error) => {
                    self.notice = Some(format!("Could not run command: {error}"));
                    false
                }
            }
        } else if action_id == "favorite" {
            self.toggle_favorite(entry_id);
            false
        } else {
            self.notice = Some("That action is not available yet".into());
            false
        }
    }

    fn toggle_favorite(&mut self, entry_id: &str) {
        let title = self.applications.get(entry_id).map_or_else(
            || "Application".into(),
            |application| application.name.clone(),
        );
        match self.preferences.toggle_favorite(entry_id) {
            Ok(favorite) => {
                self.apply_saved_preference(entry_id);
                self.notice = Some(if favorite {
                    format!("Added {title} to favorites")
                } else {
                    format!("Removed {title} from favorites")
                });
            }
            Err(error) => self.notice = Some(format!("Could not update favorite: {error}")),
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
            self.notice = Some(format!("Could not save launcher preferences: {error}"));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "surface result composition is easiest to audit in one dispatch function"
    )]
    fn refresh_results(&mut self, cx: &mut Context<Self>) {
        self.files.clear();
        self.calculations.clear();
        if self.surface == PaletteSurface::Clipboard {
            let query = self.model.query().trim().to_owned();
            let entries = self
                .clipboard
                .as_mut()
                .ok_or_else(|| "clipboard history is unavailable".to_owned())
                .and_then(|history| history.search(&query));
            match entries {
                Ok(entries) => self.model.replace_entries(entries),
                Err(error) => {
                    self.notice = Some(format!("Could not load clipboard history: {error}"));
                    self.model.replace_entries(Vec::new());
                }
            }
            return;
        }
        if self.surface == PaletteSurface::Tools {
            let query = self.model.query().trim().to_owned();
            let entries = self
                .mini_tools
                .as_mut()
                .ok_or_else(|| "mini tools are unavailable".to_owned())
                .and_then(|tools| tools.entries(&query));
            match entries {
                Ok(entries) => self.model.replace_entries(entries),
                Err(error) => {
                    self.notice = Some(format!("Could not load mini tools: {error}"));
                    self.model.replace_entries(Vec::new());
                }
            }
            return;
        }
        if self.surface == PaletteSurface::Emoji {
            self.model
                .replace_entries(MiniTools::emoji_entries(self.model.query()));
            return;
        }
        if self.surface == PaletteSurface::Currency {
            self.refresh_currency(cx);
            return;
        }
        let query = self.model.query().trim().to_owned();
        if CurrencyQuery::parse(&query).is_some() {
            self.refresh_currency(cx);
            return;
        }
        self.currency_query = None;
        self.currency_input = None;
        self.currency_result = None;
        let keyword_entry = self
            .mini_tools
            .as_mut()
            .and_then(|tools| tools.keyword_entry(&query).ok().flatten());
        let matches = if query.chars().count() >= 2 {
            self.file_index
                .as_ref()
                .and_then(|index| index.search(&query, None, 30).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut entries = self.base_entries.clone();
        entries.extend(keyword_entry);
        if let Some(result) = calculate(&query) {
            let id = format!("calculation:{query}");
            self.calculations.insert(id.clone(), result.clone());
            entries.push(PaletteEntry {
                id,
                title: result,
                subtitle: query.clone(),
                kind: PaletteEntryKind::Calculation,
                icon: None,
                keywords: vec![query.clone(), "calculator".into()],
                preview: "Press Enter to copy the result".into(),
                frequency: u32::MAX,
                favorite: true,
                actions: vec![ActionItem {
                    id: "copy-result".into(),
                    title: "Copy Result".into(),
                    shortcut: Some("↵".into()),
                }],
            });
        }
        entries.extend(matches.into_iter().map(|matched| {
            let id = format!("file:{}", matched.path.display());
            self.files.insert(id.clone(), matched.path.clone());
            let subtitle = matched
                .path
                .parent()
                .map_or_else(String::new, |parent| parent.display().to_string());
            PaletteEntry {
                id,
                title: matched.name,
                subtitle,
                kind: PaletteEntryKind::File,
                icon: None,
                keywords: vec![query.clone()],
                preview: format!("{} bytes", matched.size),
                frequency: 0,
                favorite: false,
                actions: vec![ActionItem {
                    id: "open-file".into(),
                    title: "Open File".into(),
                    shortcut: Some("↵".into()),
                }],
            }
        }));
        self.model.replace_entries(entries);
    }

    fn enter_surface(&mut self, surface: PaletteSurface, cx: &mut Context<Self>) {
        self.surface = surface;
        self.notice = None;
        let placeholder = match surface {
            PaletteSurface::Launcher => "Search apps, files, and tools…",
            PaletteSurface::Clipboard => "Search clipboard history…",
            PaletteSurface::Tools => "Search mini tools…",
            PaletteSurface::Emoji => "Search emoji by name…",
            PaletteSurface::Currency => "Try 100 USD to EUR or 0.1 BTC in USD…",
        };
        self.search_input
            .update(cx, |input, cx| input.reset(placeholder, cx));
        self.results_scroll.scroll_to_item(0);
        self.refresh_results(cx);
    }

    fn refresh_currency(&mut self, cx: &mut Context<Self>) {
        let Some(query) = CurrencyQuery::parse(self.model.query()) else {
            self.currency_query = None;
            self.currency_input = None;
            self.currency_result = None;
            self.model.replace_entries(Vec::new());
            return;
        };
        self.currency_input = Some(self.model.query().to_owned());
        if self.currency_query.as_ref() == Some(&query) {
            self.show_currency_result();
            return;
        }

        self.currency_query = Some(query.clone());
        self.currency_result = None;
        self.model.replace_entries(vec![currency_loading_entry(
            &query,
            self.currency_input.as_deref().unwrap_or_default(),
        )]);
        let root = data_root();
        let requested_query = query.clone();
        cx.spawn(async move |this, cx| {
            let task = cx
                .background_executor()
                .spawn(async move { currency::convert(query.clone(), &root) });
            let result = task.await;
            let _ = this.update(cx, |palette, cx| {
                if palette.currency_query.as_ref() == Some(&requested_query) {
                    palette.currency_result = Some(result);
                    palette.show_currency_result();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn show_currency_result(&mut self) {
        let entries = match &self.currency_result {
            None => self
                .currency_query
                .as_ref()
                .map(|query| {
                    currency_loading_entry(
                        query,
                        self.currency_input.as_deref().unwrap_or_default(),
                    )
                })
                .into_iter()
                .collect(),
            Some(Ok(result)) => vec![currency_result_entry(
                result,
                self.currency_input.as_deref().unwrap_or_default(),
            )],
            Some(Err(error)) => {
                self.notice = Some(error.clone());
                Vec::new()
            }
        };
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
        let colors = theme::for_kind(self.theme_kind);
        let matches = self.model.results().cloned().collect::<Vec<_>>();
        let selected = self.model.selected_entry().cloned();
        let selected_index = self.model.selected_index();
        let action_mode = self.model.mode() == PaletteMode::Actions;
        let section_title = if action_mode {
            selected.as_ref().map_or_else(
                || "Actions".into(),
                |entry| format!("Actions for {}", entry.title),
            )
        } else if self.surface != PaletteSurface::Launcher {
            match self.surface {
                PaletteSurface::Clipboard => "Clipboard History",
                PaletteSurface::Tools => "Mini Tools",
                PaletteSurface::Emoji => "Emoji Picker",
                PaletteSurface::Currency => "Currency & Crypto",
                PaletteSurface::Launcher => unreachable!(),
            }
            .into()
        } else if self.model.query().is_empty() {
            "Suggestions".into()
        } else {
            "Results".into()
        };

        let rows = if action_mode {
            self.model
                .actions()
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    action_row(index, action, index == selected_index, colors, cx)
                })
                .collect::<Vec<_>>()
        } else {
            matches
                .iter()
                .enumerate()
                .map(|(index, entry)| result_row(index, entry, index == selected_index, colors, cx))
                .collect::<Vec<_>>()
        };

        let footer_label = self.notice.clone().unwrap_or_else(|| {
            if self.surface != PaletteSurface::Launcher {
                match self.surface {
                    PaletteSurface::Clipboard => format!("{} clipboard items", matches.len()),
                    PaletteSurface::Tools => format!("{} tools", matches.len()),
                    PaletteSurface::Emoji => format!("{} emoji", matches.len()),
                    PaletteSurface::Currency => "Live rates · cached for offline use".into(),
                    PaletteSurface::Launcher => unreachable!(),
                }
            } else if self.model.query().is_empty() {
                format!("{} applications", self.application_count)
            } else {
                format!("{} results", matches.len())
            }
        });

        div()
            .id("superspace-palette")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.text)
            .rounded(px(20.0))
            .shadow(vec![BoxShadow {
                color: colors.shadow,
                offset: point(px(0.0), px(12.0)),
                blur_radius: px(36.0),
                spread_radius: px(-8.0),
                inset: false,
            }])
            .overflow_hidden()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .child(
                div().h(px(52.0)).px(px(16.0)).flex().items_center().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(38.0))
                        .child(self.search_input.clone()),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px(px(8.0))
                    .pt(px(5.0))
                    .pb(px(4.0))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(22.0))
                            .px(px(6.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.muted)
                            .child(section_title)
                            .child(if action_mode { "ESC TO GO BACK" } else { "" }),
                    )
                    .child(
                        div()
                            .id("results-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.results_scroll)
                            .when(rows.is_empty(), |list| {
                                list.child(
                                    div()
                                        .size_full()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .gap(px(7.0))
                                        .child(
                                            div()
                                                .text_size(px(14.0))
                                                .font_weight(FontWeight::MEDIUM)
                                                .child(empty_title(self.surface)),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.0))
                                                .text_color(colors.muted)
                                                .child(empty_hint(self.surface)),
                                        ),
                                )
                            })
                            .children(rows),
                    ),
            )
            .child(
                div()
                    .h(px(38.0))
                    .px(px(12.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(11.0))
                            .text_color(colors.muted)
                            .child(footer_label),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .text_size(px(11.0))
                            .text_color(colors.muted)
                            .child(if action_mode { "Run" } else { "Open" })
                            .child(keycap("↵", colors))
                            .when(!action_mode && selected.is_some(), |footer| {
                                footer.child("Actions").child(keycap("⌘ K", colors))
                            }),
                    ),
            )
            .with_animation(
                "palette-enter",
                motion::ENTER.animation(),
                |element, progress| {
                    element
                        .opacity(progress)
                        .relative()
                        .top(px(4.0 * (1.0 - progress)))
                },
            )
    }
}

fn result_row(
    index: usize,
    entry: &PaletteEntry,
    selected: bool,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    let entry = entry.clone();
    if entry.kind == PaletteEntryKind::Calculation {
        return calculation_row(index, entry, selected, colors, cx);
    }
    div()
        .id(("result-row", index))
        .h(px(40.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(9.0))
        .rounded(px(8.0))
        .when(selected, |row| row.bg(colors.selected))
        .hover(move |row| {
            row.bg(if selected {
                colors.selected
            } else {
                colors.hovered
            })
        })
        .on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    let palette_event = this.model.invoke(index);
                    this.handle_event(palette_event, window, cx);
                } else {
                    this.model.select(index);
                }
                cx.notify();
            }),
        )
        .child(entry_icon(&entry, colors))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(7.0))
                .child(
                    div()
                        .flex_shrink_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(entry.title),
                )
                .when(!entry.subtitle.is_empty(), |content| {
                    content.child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_size(px(10.0))
                            .text_color(colors.muted)
                            .child(entry.subtitle),
                    )
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(11.0))
                .text_color(colors.muted)
                .child(entry.kind.label()),
        )
        .into_any_element()
}

fn calculation_row(
    index: usize,
    entry: PaletteEntry,
    selected: bool,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    div()
        .id(("calculation-row", index))
        .h(px(78.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .gap(px(14.0))
        .rounded(px(8.0))
        .bg(if selected {
            colors.selected
        } else {
            colors.surface
        })
        .hover(move |row| {
            row.bg(if selected {
                colors.selected
            } else {
                colors.hovered
            })
        })
        .on_click(
            cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    let palette_event = this.model.invoke(index);
                    this.handle_event(palette_event, window, cx);
                } else {
                    this.model.select(index);
                }
                cx.notify();
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(14.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(17.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(entry.subtitle),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(colors.muted)
                        .child("→"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(21.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(entry.title),
                ),
        )
        .into_any_element()
}

fn action_row(
    index: usize,
    action: &ActionItem,
    selected: bool,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    let shortcut = action.shortcut.clone();
    div()
        .id(("action-row", index))
        .h(px(40.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .gap(px(9.0))
        .rounded(px(8.0))
        .when(selected, |row| row.bg(colors.selected))
        .hover(move |row| {
            row.bg(if selected {
                colors.selected
            } else {
                colors.hovered
            })
        })
        .on_click(cx.listener(move |this, _, window, cx| {
            let palette_event = this.model.invoke(index);
            this.handle_event(palette_event, window, cx);
            cx.notify();
        }))
        .child(div().w(px(24.0)).text_color(colors.muted).child("›"))
        .child(
            div()
                .flex_1()
                .text_size(px(13.0))
                .font_weight(FontWeight::MEDIUM)
                .child(action.title.clone()),
        )
        .when_some(shortcut, |row, shortcut| {
            row.child(keycap(shortcut, colors))
        })
        .into_any_element()
}

fn entry_icon(entry: &PaletteEntry, colors: theme::Theme) -> AnyElement {
    let label = match entry.kind {
        PaletteEntryKind::Application => entry
            .title
            .chars()
            .next()
            .unwrap_or('A')
            .to_uppercase()
            .to_string(),
        PaletteEntryKind::File => "F".into(),
        PaletteEntryKind::Command => "›".into(),
        PaletteEntryKind::Calculation => "=".into(),
        PaletteEntryKind::Clipboard => "⎘".into(),
        PaletteEntryKind::Tool => "◇".into(),
        PaletteEntryKind::Emoji => entry.title.clone(),
    };
    if let Some(path) = entry.icon.as_ref().filter(|path| is_renderable_image(path)) {
        let fallback_label = label.clone();
        return img(path.clone())
            .size(px(24.0))
            .rounded(px(5.0))
            .with_fallback(move || fallback_icon(fallback_label.clone(), colors))
            .into_any_element();
    }
    fallback_icon(label, colors)
}

fn fallback_icon(label: String, colors: theme::Theme) -> AnyElement {
    div()
        .size(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .bg(colors.tile)
        .text_size(px(10.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors.muted)
        .child(label)
        .into_any_element()
}

fn keycap(label: impl Into<String>, colors: theme::Theme) -> AnyElement {
    div()
        .h(px(18.0))
        .px(px(5.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .bg(colors.surface)
        .text_size(px(9.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors.muted)
        .child(label.into())
        .into_any_element()
}

fn is_renderable_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            gpui::Img::extensions()
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

fn currency_loading_entry(query: &CurrencyQuery, input: &str) -> PaletteEntry {
    PaletteEntry {
        id: "currency:loading".into(),
        title: format!("Fetching {} → {}…", query.from, query.to),
        subtitle: "Checking the latest exchange rate".into(),
        kind: PaletteEntryKind::Calculation,
        icon: None,
        keywords: vec![input.to_owned()],
        preview: "Live currency conversion".into(),
        frequency: 100,
        favorite: true,
        actions: Vec::new(),
    }
}

fn currency_result_entry(result: &Conversion, input: &str) -> PaletteEntry {
    let value = result.display_value();
    let source = if result.cached {
        "Cached rate"
    } else {
        "Live rate"
    };
    let observed = chrono::DateTime::from_timestamp_millis(result.observed_at_ms).map_or_else(
        || source.to_owned(),
        |time| format!("{source} · {}", time.format("%b %-d, %H:%M UTC")),
    );
    PaletteEntry {
        id: "currency:result".into(),
        title: format!("{value} {}", result.query.to),
        subtitle: format!("{} {} · {observed}", result.query.amount, result.query.from),
        kind: PaletteEntryKind::Calculation,
        icon: None,
        keywords: vec![
            input.to_owned(),
            result.query.from.to_string(),
            result.query.to.to_string(),
        ],
        preview: "Press Enter to copy the converted amount".into(),
        frequency: u32::MAX,
        favorite: true,
        actions: vec![ActionItem {
            id: "copy-currency".into(),
            title: "Copy Converted Amount".into(),
            shortcut: Some("↵".into()),
        }],
    }
}

const fn empty_title(surface: PaletteSurface) -> &'static str {
    match surface {
        PaletteSurface::Currency => "Type a conversion",
        PaletteSurface::Emoji => "No emoji found",
        PaletteSurface::Tools => "No tools found",
        PaletteSurface::Clipboard => "No clipboard items",
        PaletteSurface::Launcher => "No matches found",
    }
}

const fn empty_hint(surface: PaletteSurface) -> &'static str {
    match surface {
        PaletteSurface::Currency => "Example: 100 USD to EUR",
        PaletteSurface::Emoji => "Try a feeling, object, or symbol",
        PaletteSurface::Tools => "Try currency, emoji, UUID, note, or command",
        PaletteSurface::Clipboard => "Copy something and it will appear here",
        PaletteSurface::Launcher => "Try an app name or a file keyword",
    }
}

fn calculate(query: &str) -> Option<String> {
    if query.is_empty()
        || !query.chars().any(|character| character.is_ascii_digit())
        || !query.chars().any(|character| {
            matches!(character, '+' | '-' | '*' | '/' | '%' | '^') || character.is_alphabetic()
        })
    {
        return None;
    }
    Calculator::default()
        .evaluate(query)
        .ok()
        .map(|result| match result {
            ResultValue::Number(value) => format_number(value),
            ResultValue::Quantity { value, unit } => {
                format!("{} {}", format_number(value), unit.symbol)
            }
        })
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.10}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

fn prepare_icon(path: &str) -> Option<PathBuf> {
    let path = PathBuf::from(path);
    if is_renderable_image(&path) {
        return Some(path);
    }
    #[cfg(target_os = "macos")]
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("icns"))
    {
        return convert_icns(&path);
    }
    None
}

#[cfg(target_os = "macos")]
fn convert_icns(path: &Path) -> Option<PathBuf> {
    use std::collections::hash_map::DefaultHasher;
    use std::fs;
    use std::hash::{Hash as _, Hasher as _};

    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let cache = data_root()
        .join("icon-cache")
        .join(format!("{:016x}.png", hasher.finish()));
    if cache.is_file() {
        return Some(cache);
    }
    fs::create_dir_all(cache.parent()?).ok()?;
    let family = icns::IconFamily::read(fs::File::open(path).ok()?).ok()?;
    let icon_type = family
        .available_icons()
        .into_iter()
        .min_by_key(|icon_type| icon_type.pixel_width().abs_diff(64))?;
    let icon = family.get_icon_with_type(icon_type).ok()?;
    icon.write_png(fs::File::create(&cache).ok()?).ok()?;
    Some(cache)
}

fn open_file_index() -> Option<superspace_files::FileIndex> {
    superspace_files::FileIndex::open(data_root().join("files.sqlite")).ok()
}

fn builtin_entry(id: &str, title: &str, preview: &str, action_id: &str) -> PaletteEntry {
    PaletteEntry {
        id: id.into(),
        title: title.into(),
        subtitle: "Superspace".into(),
        kind: PaletteEntryKind::Command,
        icon: None,
        keywords: vec![title.into(), preview.into(), "tool".into()],
        preview: preview.into(),
        frequency: 100,
        favorite: true,
        actions: vec![ActionItem {
            id: action_id.into(),
            title: title.into(),
            shortcut: Some("↵".into()),
        }],
    }
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

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::{currency_loading_entry, currency_result_entry};
    use crate::{PaletteModel, currency::Conversion};
    use superspace_calculator::CurrencyQuery;

    #[test]
    fn currency_rows_remain_visible_for_the_complete_typed_query() {
        let input = "1,250.50 usd to EUR";
        let query = CurrencyQuery::parse(input).expect("currency query");
        let mut loading = PaletteModel::new(vec![currency_loading_entry(&query, input)]);
        loading.set_query(input);
        assert!(loading.results().next().is_some());

        let conversion = Conversion {
            query,
            value: Decimal::new(1_154_250, 3),
            observed_at_ms: 42,
            cached: false,
        };
        let mut result = PaletteModel::new(vec![currency_result_entry(&conversion, input)]);
        result.set_query(input);
        assert!(result.results().next().is_some());
    }
}
