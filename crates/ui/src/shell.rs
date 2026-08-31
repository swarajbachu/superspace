use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, App, AppContext as _, BoxShadow, ClipboardItem, Context, Entity,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, point, px,
};
use superspace_calculator::{Calculator, ResultValue};
use superspace_core::LauncherPreferences;
use superspace_platform::AppDescriptor;

use crate::{
    ActionItem, PaletteEntry, PaletteEntryKind, PaletteEvent, PaletteKey, PaletteMode,
    PaletteModel, motion,
    search_input::{InputChanged, SearchInput},
    theme,
};

/// Main command palette state.
pub struct Palette {
    model: PaletteModel,
    focus: FocusHandle,
    search_input: Entity<SearchInput>,
    results_scroll: ScrollHandle,
    notice: Option<String>,
    application_count: usize,
    applications: HashMap<String, AppDescriptor>,
    base_entries: Vec<PaletteEntry>,
    file_index: Option<superspace_files::FileIndex>,
    files: HashMap<String, PathBuf>,
    calculations: HashMap<String, String>,
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
        let entries = applications
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
        let base_entries = entries.clone();

        cx.subscribe(&search_input, |palette, input, _: &InputChanged, cx| {
            palette.model.set_query(input.read(cx).text().to_owned());
            palette.notice = None;
            palette.refresh_results();
            palette.results_scroll.scroll_to_item(0);
            cx.notify();
        })
        .detach();

        Self {
            model: PaletteModel::new(entries),
            focus,
            search_input,
            results_scroll,
            notice: None,
            application_count,
            applications,
            base_entries,
            file_index: open_file_index(),
            files: HashMap::new(),
            calculations: HashMap::new(),
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
        let palette_event = self.model.key(key);
        self.results_scroll
            .scroll_to_item(self.model.selected_index());
        self.handle_event(palette_event, window, cx);
        cx.stop_propagation();
        cx.notify();
    }

    /// Returns whether the palette should close after the action.
    fn invoke(&mut self, entry_id: &str, action_id: &str, cx: &mut Context<Self>) -> bool {
        if action_id == "launch" {
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

    fn refresh_results(&mut self) {
        self.files.clear();
        self.calculations.clear();
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
        if let Some(result) = calculate(query) {
            let id = format!("calculation:{query}");
            self.calculations.insert(id.clone(), result.clone());
            entries.push(PaletteEntry {
                id,
                title: result,
                subtitle: query.to_owned(),
                kind: PaletteEntryKind::Calculation,
                icon: None,
                keywords: vec![query.to_owned(), "calculator".into()],
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
                keywords: Vec::new(),
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
            if self.model.query().is_empty() {
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
                                                .child("No matches found"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(12.0))
                                                .text_color(colors.muted)
                                                .child("Try an app name or a file keyword"),
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
