use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyElement, App, AppContext as _, BoxShadow, ClipboardItem, Context, Entity,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ListAlignment, ListState, ParentElement as _, Render, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, hsla, img,
    linear_color_stop, linear_gradient, list, point, px, size, svg,
};
use superspace_calculator::{Calculator, CurrencyQuery, ResultValue, TimeQuery};
use superspace_core::LauncherPreferences;
use superspace_network::{DiscoveryEvent, NearbyDevice, NearbyDiscovery};
use superspace_platform::{AppDescriptor, LocaleDefaults};
use superspace_productivity::resolve_quicklink;
use superspace_storage::{ClipboardKind, TrustedDevice, TrustedDeviceStore};

use crate::{
    ActionItem, PaletteEntry, PaletteEntryKind, PaletteEvent, PaletteKey, PaletteMode,
    PaletteModel,
    clipboard_history::{ClipboardHistory, ClipboardPreview},
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
    Emoji,
    Currency,
    Nearby,
}

enum PairingEvent {
    Code(superspace_network::PairingCode, mpsc::Sender<bool>),
    Finished(Result<String, String>),
}

const EMOJI_COLUMNS: usize = 8;
const EMOJI_COLUMN_DELTA: isize = 8;
const CURRENCY_PREFIXES: &[&str] = &[
    "u", "us", "e", "eu", "g", "gb", "i", "in", "j", "jp", "c", "ca", "a", "au", "b", "bt", "et",
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ClipboardFilter {
    #[default]
    All,
    Text,
    Images,
    Files,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum EmojiCategory {
    #[default]
    All,
    SmileysAndPeople,
    AnimalsAndNature,
    FoodAndDrink,
    TravelAndPlaces,
    Activities,
    Objects,
    Symbols,
    Flags,
}

impl EmojiCategory {
    const ALL: [Self; 9] = [
        Self::All,
        Self::SmileysAndPeople,
        Self::AnimalsAndNature,
        Self::FoodAndDrink,
        Self::TravelAndPlaces,
        Self::Activities,
        Self::Objects,
        Self::Symbols,
        Self::Flags,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::SmileysAndPeople => "Smileys & People",
            Self::AnimalsAndNature => "Animals & Nature",
            Self::FoodAndDrink => "Food & Drink",
            Self::TravelAndPlaces => "Travel & Places",
            Self::Activities => "Activities",
            Self::Objects => "Objects",
            Self::Symbols => "Symbols",
            Self::Flags => "Flags",
        }
    }

    const fn tag(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::SmileysAndPeople => Some("emoji-group:smileys-people"),
            Self::AnimalsAndNature => Some("emoji-group:animals-nature"),
            Self::FoodAndDrink => Some("emoji-group:food-drink"),
            Self::TravelAndPlaces => Some("emoji-group:travel-places"),
            Self::Activities => Some("emoji-group:activities"),
            Self::Objects => Some("emoji-group:objects"),
            Self::Symbols => Some("emoji-group:symbols"),
            Self::Flags => Some("emoji-group:flags"),
        }
    }
}

#[derive(Clone, Debug)]
enum EmojiRow {
    Header { label: &'static str, count: usize },
    Tiles(Vec<usize>),
}

impl EmojiRow {
    fn contains_entry(&self, index: usize) -> bool {
        matches!(self, Self::Tiles(indices) if indices.contains(&index))
    }
}

impl ClipboardFilter {
    const ALL: [Self; 4] = [Self::All, Self::Text, Self::Images, Self::Files];

    const fn next(self) -> Self {
        match self {
            Self::All => Self::Text,
            Self::Text => Self::Images,
            Self::Images => Self::Files,
            Self::Files => Self::All,
        }
    }

    const fn previous(self) -> Self {
        match self {
            Self::All => Self::Files,
            Self::Text => Self::All,
            Self::Images => Self::Text,
            Self::Files => Self::Images,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::All => "All Types",
            Self::Text => "Text",
            Self::Images => "Images",
            Self::Files => "Files",
        }
    }

    const fn kind(self) -> Option<ClipboardKind> {
        match self {
            Self::All => None,
            Self::Text => Some(ClipboardKind::Text),
            Self::Images => Some(ClipboardKind::Image),
            Self::Files => Some(ClipboardKind::Files),
        }
    }
}

/// Main command palette state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent popup and nearby activity flags do not form one state machine"
)]
pub struct Palette {
    model: PaletteModel,
    focus: FocusHandle,
    search_input: Entity<SearchInput>,
    results_scroll: ScrollHandle,
    emoji_scroll: ListState,
    emoji_rows: Vec<EmojiRow>,
    emoji_category: EmojiCategory,
    emoji_category_open: bool,
    surface: PaletteSurface,
    notice: Option<String>,
    application_count: usize,
    applications: HashMap<String, AppDescriptor>,
    base_entries: Vec<PaletteEntry>,
    file_index: Option<superspace_files::FileIndex>,
    files: HashMap<String, PathBuf>,
    calculations: HashMap<String, String>,
    clipboard: Option<ClipboardHistory>,
    clipboard_filter: ClipboardFilter,
    clipboard_filter_open: bool,
    mini_tools: Option<MiniTools>,
    currency_query: Option<CurrencyQuery>,
    currency_input: Option<String>,
    currency_result: Option<Result<Conversion, String>>,
    theme_kind: theme::ThemeKind,
    preferences: LauncherPreferences,
    preferences_path: PathBuf,
    focused_text_target: bool,
    locale: LocaleDefaults,
    browser_name: String,
    nearby_devices: Vec<TrustedDevice>,
    discovered_devices: HashMap<uuid::Uuid, NearbyDevice>,
    nearby_discovery: Option<NearbyDiscovery>,
    local_device_id: Option<uuid::Uuid>,
    pairing_code: Option<String>,
    pairing_confirmation: Option<mpsc::Sender<bool>>,
    pairing_active: bool,
    nearby_processes: Vec<Child>,
}

impl Palette {
    #[allow(
        clippy::too_many_lines,
        reason = "palette construction keeps persisted feature sources visible in one composition root"
    )]
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<Self>,
        focused_text_target: bool,
    ) -> Self {
        let search_input = cx.new(SearchInput::new);
        let results_scroll = ScrollHandle::new();
        let emoji_scroll = ListState::new(0, ListAlignment::Top, px(120.0));
        let focus = search_input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        let locale = superspace_platform::locale_defaults();
        let browser_name =
            superspace_platform::default_browser_name().unwrap_or_else(|| "your browser".into());

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
            "builtin:nearby",
            "Nearby Sharing",
            "Pair devices, sync clipboard, and share files",
            "open-nearby",
        ));
        let clipboard = ClipboardHistory::open(&data_root()).ok();
        let local_identity = superspace_network::LocalIdentity::load_or_create(
            data_root().join("local-identity.cbor"),
        )
        .ok();
        let local_device_id = local_identity.as_ref().map(|identity| identity.device_id);
        let nearby_discovery = local_identity.as_ref().and_then(|identity| {
            NearbyDiscovery::start(
                identity.device_id,
                platform_device_name(),
                &discovery_host_label(identity.device_id),
                43870,
                identity.noise.public_key(),
            )
            .ok()
        });
        let nearby_devices = TrustedDeviceStore::open(data_root().join("trusted-devices.sqlite"))
            .and_then(|store| store.list())
            .unwrap_or_default();
        let mut mini_tools = MiniTools::open(&data_root()).ok();
        if let Some(tools) = mini_tools.as_mut()
            && let Ok(tool_entries) = tools.entries("")
        {
            entries.extend(tool_entries);
        }
        let base_entries = entries.clone();

        cx.subscribe(&search_input, |palette, input, _: &InputChanged, cx| {
            palette.model.set_query(input.read(cx).text().to_owned());
            palette.notice = None;
            palette.refresh_results(cx);
            palette.results_scroll.scroll_to_item(0);
            cx.notify();
        })
        .detach();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(350))
                    .await;
                if this
                    .update(cx, |palette, cx| {
                        let mut changed = false;
                        if let Some(discovery) = &palette.nearby_discovery {
                            for event in discovery.poll() {
                                match event {
                                    DiscoveryEvent::Resolved(device)
                                        if Some(device.id) != palette.local_device_id =>
                                    {
                                        palette.discovered_devices.insert(device.id, device);
                                        changed = true;
                                    }
                                    DiscoveryEvent::Removed(fullname) => {
                                        if let Some(id) = discovery_id_from_fullname(&fullname) {
                                            changed |=
                                                palette.discovered_devices.remove(&id).is_some();
                                        }
                                    }
                                    DiscoveryEvent::Resolved(_) => {}
                                }
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            model: PaletteModel::new(entries),
            focus,
            search_input,
            results_scroll,
            emoji_scroll,
            emoji_rows: Vec::new(),
            emoji_category: EmojiCategory::default(),
            emoji_category_open: false,
            surface: PaletteSurface::Launcher,
            notice: None,
            application_count,
            applications,
            base_entries,
            file_index: open_file_index(),
            files: HashMap::new(),
            calculations: HashMap::new(),
            clipboard,
            clipboard_filter: ClipboardFilter::default(),
            clipboard_filter_open: false,
            mini_tools,
            currency_query: None,
            currency_input: None,
            currency_result: None,
            theme_kind: theme::ThemeKind::default(),
            preferences,
            preferences_path,
            focused_text_target,
            locale,
            browser_name,
            nearby_devices,
            discovered_devices: HashMap::new(),
            nearby_discovery,
            local_device_id,
            pairing_code: None,
            pairing_confirmation: None,
            pairing_active: false,
            nearby_processes: Vec::new(),
        }
    }

    fn handle_event(&mut self, event: PaletteEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PaletteEvent::None => {}
            PaletteEvent::Invoke {
                entry_id,
                action_id,
            } => {
                if self.invoke(&entry_id, &action_id, window, cx) {
                    window.remove_window();
                }
            }
            PaletteEvent::Dismiss => window.remove_window(),
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if self.surface == PaletteSurface::Nearby && keystroke.key == "enter" {
            self.start_pairing(false, None, cx);
            cx.stop_propagation();
            return;
        }
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
        if self.surface == PaletteSurface::Clipboard && self.clipboard_filter_open {
            match keystroke.key.as_str() {
                "up" => self.clipboard_filter = self.clipboard_filter.previous(),
                "down" => self.clipboard_filter = self.clipboard_filter.next(),
                "enter" | "escape" => {
                    self.clipboard_filter_open = false;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                _ => return,
            }
            self.refresh_results(cx);
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
            self.enter_surface(PaletteSurface::Launcher, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.surface == PaletteSurface::Emoji && self.model.mode() == PaletteMode::Results {
            let offset = match key {
                PaletteKey::Up => Some(-EMOJI_COLUMN_DELTA),
                PaletteKey::Down => Some(EMOJI_COLUMN_DELTA),
                _ => None,
            };
            if let Some(offset) = offset {
                self.model.move_selection_by(offset);
                self.reveal_selected_emoji();
                cx.stop_propagation();
                cx.notify();
                return;
            }
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
    fn invoke(
        &mut self,
        entry_id: &str,
        action_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if action_id == "open-clipboard" {
            self.enter_surface(PaletteSurface::Clipboard, window, cx);
            false
        } else if action_id == "open-emoji" {
            self.enter_surface(PaletteSurface::Emoji, window, cx);
            false
        } else if action_id == "open-currency" {
            self.enter_surface(PaletteSurface::Currency, window, cx);
            false
        } else if action_id == "open-nearby" {
            self.enter_surface(PaletteSurface::Nearby, window, cx);
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
        } else if action_id == "search-web" {
            let url = resolve_quicklink(
                "https://www.google.com/search?q={query}",
                self.model.query(),
            );
            match superspace_platform::open_path(url) {
                Ok(_) => true,
                Err(error) => {
                    self.notice = Some(format!("Could not open web search: {error}"));
                    false
                }
            }
        } else if action_id == "search-files" {
            let folder = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
            match superspace_platform::open_path(folder) {
                Ok(_) => true,
                Err(error) => {
                    self.notice = Some(format!("Could not open file browser: {error}"));
                    false
                }
            }
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
        } else if action_id == "copy-emoji" || action_id == "copy-symbol" {
            if let Some(value) = entry_id
                .strip_prefix("emoji:")
                .or_else(|| entry_id.strip_prefix("symbol:"))
            {
                cx.write_to_clipboard(ClipboardItem::new_string(value.to_owned()));
                self.notice = Some(format!("Copied {value}"));
            }
            if self.focused_text_target {
                cx.hide();
                let _ = superspace_platform::paste_from_clipboard();
            }
            true
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
        if self.surface == PaletteSurface::Nearby {
            self.model.replace_entries(Vec::new());
            return;
        }
        if self.surface == PaletteSurface::Clipboard {
            let query = self.model.query().trim().to_owned();
            let entries = self
                .clipboard
                .as_mut()
                .ok_or_else(|| "clipboard history is unavailable".to_owned())
                .and_then(|history| history.search(&query, self.clipboard_filter.kind()));
            match entries {
                Ok(entries) => self.model.replace_entries(entries),
                Err(error) => {
                    self.notice = Some(format!("Could not load clipboard history: {error}"));
                    self.model.replace_entries(Vec::new());
                }
            }
            return;
        }
        if self.surface == PaletteSurface::Emoji {
            let mut entries = MiniTools::emoji_entries(self.model.query());
            if let Some(tag) = self.emoji_category.tag() {
                entries.retain(|entry| {
                    !entry
                        .keywords
                        .iter()
                        .any(|keyword| keyword == "emoji-group:frequent")
                        && entry.keywords.iter().any(|keyword| keyword == tag)
                });
            }
            self.model.replace_entries_ordered(entries);
            self.rebuild_emoji_rows();
            return;
        }
        if self.surface == PaletteSurface::Currency {
            self.refresh_currency(cx);
            return;
        }
        let query = self.model.query().trim().to_owned();
        if parse_currency_for_locale(&query, &self.locale.currency).is_some() {
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
        let matches = if query.chars().count() >= 1 {
            self.file_index
                .as_ref()
                .and_then(|index| index.search(&query, None, 30).ok())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut entries = self.base_entries.clone();
        entries.extend(keyword_entry);
        if let Some(time) =
            TimeQuery::parse(&query, &self.locale.time_zone).and_then(|query| query.convert().ok())
        {
            let output = format!("{} {}", time.output_time, time.to_zone);
            let input = format!("{} {}", time.input_time, time.from_zone);
            let day = match time.day_offset {
                -1 => " · previous day",
                1 => " · next day",
                _ => "",
            };
            let id = format!("time:{query}");
            self.calculations.insert(id.clone(), output.clone());
            entries.push(PaletteEntry {
                id,
                title: output,
                subtitle: input,
                kind: PaletteEntryKind::Calculation,
                icon: None,
                keywords: vec![query.clone(), "time zone conversion".into()],
                preview: format!("Local time{day}"),
                frequency: u32::MAX,
                favorite: true,
                actions: vec![ActionItem {
                    id: "copy-result".into(),
                    title: "Copy Time".into(),
                    shortcut: Some("↵".into()),
                }],
            });
        }
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
                preview: "Calculation".into(),
                frequency: u32::MAX,
                favorite: true,
                actions: vec![ActionItem {
                    id: "copy-result".into(),
                    title: "Copy Result".into(),
                    shortcut: Some("↵".into()),
                }],
            });
        } else if let Some(intent) = tool_intent(&query) {
            entries.push(intent_entry(intent, &query));
        }
        let completed_tools = entries
            .iter()
            .filter(|entry| {
                entry.kind == PaletteEntryKind::Calculation && !entry.actions.is_empty()
            })
            .cloned()
            .collect::<Vec<_>>();
        if !completed_tools.is_empty() {
            let mut focused_entries = completed_tools;
            focused_entries.extend(fallback_entries(&query, &self.browser_name));
            self.model.replace_entries_ordered(focused_entries);
            return;
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
        if !query.is_empty() && self.model.results().next().is_none() {
            self.model
                .replace_entries_ordered(fallback_entries(&query, &self.browser_name));
        }
    }

    fn enter_surface(
        &mut self,
        surface: PaletteSurface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.surface = surface;
        window.resize(if surface == PaletteSurface::Emoji {
            size(px(620.0), px(500.0))
        } else {
            size(px(800.0), px(500.0))
        });
        self.clipboard_filter_open = false;
        self.emoji_category_open = false;
        self.notice = None;
        let placeholder = match surface {
            PaletteSurface::Launcher => "Search apps, files, and tools…",
            PaletteSurface::Clipboard => "Search clipboard history…",
            PaletteSurface::Emoji => "Search emoji and symbols…",
            PaletteSurface::Currency => "Try 100 USD to EUR or 0.1 BTC in USD…",
            PaletteSurface::Nearby => "Enter IP address…",
        };
        self.search_input
            .update(cx, |input, cx| input.reset(placeholder, cx));
        self.results_scroll.scroll_to_item(0);
        self.refresh_results(cx);
    }

    fn refresh_nearby_devices(&mut self) {
        self.nearby_devices = TrustedDeviceStore::open(data_root().join("trusted-devices.sqlite"))
            .and_then(|store| store.list())
            .unwrap_or_default();
    }

    fn start_pairing(
        &mut self,
        listen: bool,
        discovered: Option<SocketAddr>,
        cx: &mut Context<Self>,
    ) {
        if self.pairing_active {
            self.notice = Some("A pairing request is already active".into());
            return;
        }
        let address = if let Some(address) = discovered {
            Ok(address)
        } else if listen {
            "0.0.0.0:43870".parse()
        } else {
            nearby_address(self.search_input.read(cx).text(), 43870)
        };
        let Ok(address) = address else {
            self.notice = Some("Enter the other computer's LAN IP first".into());
            cx.notify();
            return;
        };
        let root = data_root();
        let (events_tx, events_rx) = mpsc::channel();
        self.pairing_active = true;
        self.pairing_code = None;
        self.notice = Some(if listen {
            "Waiting for a pairing request on port 43870…".into()
        } else {
            format!("Connecting to {address}…")
        });
        thread::spawn(move || {
            let result = run_ui_pairing(&root, listen, address, &events_tx);
            let _ = events_tx.send(PairingEvent::Finished(result));
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let mut finished = false;
                while let Ok(event) = events_rx.try_recv() {
                    let _ = this.update(cx, |palette, cx| {
                        match event {
                            PairingEvent::Code(code, confirmation) => {
                                palette.pairing_code = Some(code.to_string());
                                palette.pairing_confirmation = Some(confirmation);
                                palette.notice = Some("Verify this code on both computers".into());
                            }
                            PairingEvent::Finished(result) => {
                                palette.pairing_active = false;
                                palette.pairing_code = None;
                                palette.pairing_confirmation = None;
                                palette.notice = Some(result.unwrap_or_else(|error| error));
                                palette.refresh_nearby_devices();
                                finished = true;
                            }
                        }
                        cx.notify();
                    });
                }
                if finished {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn confirm_pairing(&mut self, approved: bool, cx: &mut Context<Self>) {
        if let Some(confirmation) = self.pairing_confirmation.take() {
            let _ = confirmation.send(approved);
            self.notice = Some(if approved {
                "Waiting for the other computer to confirm…".into()
            } else {
                "Pairing rejected".into()
            });
            cx.notify();
        }
    }

    fn toggle_nearby_device(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        let enabled = self
            .nearby_devices
            .iter()
            .find(|device| device.id == id)
            .is_some_and(|device| !device.enabled);
        match TrustedDeviceStore::open(data_root().join("trusted-devices.sqlite"))
            .and_then(|store| store.set_enabled(id, enabled))
        {
            Ok(true) => {
                self.notice = Some(
                    if enabled {
                        "Device enabled"
                    } else {
                        "Device paused"
                    }
                    .into(),
                );
                self.refresh_nearby_devices();
            }
            _ => self.notice = Some("Could not update the paired device".into()),
        }
        cx.notify();
    }

    fn forget_nearby_device(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
        let result = TrustedDeviceStore::open(data_root().join("trusted-devices.sqlite"))
            .and_then(|store| store.remove(id));
        self.notice = Some(if matches!(result, Ok(true)) {
            "Device forgotten".into()
        } else {
            "Could not forget the paired device".into()
        });
        self.refresh_nearby_devices();
        cx.notify();
    }

    fn start_nearby_process(
        &mut self,
        action: &str,
        peer_id: uuid::Uuid,
        listen: bool,
        discovered: Option<SocketAddr>,
        cx: &mut Context<Self>,
    ) {
        let port = if action == "clipboard" { 43871 } else { 43872 };
        let address = if listen {
            format!("0.0.0.0:{port}")
        } else if let Some(mut address) = discovered {
            address.set_port(port);
            address.to_string()
        } else {
            let Ok(address) = nearby_address(self.search_input.read(cx).text(), port) else {
                self.notice = Some("Enter the other computer's LAN IP first".into());
                cx.notify();
                return;
            };
            address.to_string()
        };
        let command = match (action, listen) {
            ("clipboard", true) => "clipboard-listen",
            ("clipboard", false) => "clipboard-connect",
            ("file", true) => "file-listen",
            _ => return,
        };
        let executable = match std::env::current_exe() {
            Ok(executable) => executable,
            Err(error) => {
                self.notice = Some(format!("Could not locate Superspace: {error}"));
                return;
            }
        };
        match Command::new(executable)
            .args([
                "nearby",
                command,
                &address,
                &peer_id.to_string(),
                "Superspace",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                self.nearby_processes.push(child);
                self.notice = Some(match (action, listen) {
                    ("clipboard", true) => "Clipboard receiver is ready".into(),
                    ("clipboard", false) => "Clipboard sync connected".into(),
                    _ => "Ready to receive one file or folder".into(),
                });
            }
            Err(error) => self.notice = Some(format!("Could not start nearby sharing: {error}")),
        }
        cx.notify();
    }

    fn send_nearby_path(
        &mut self,
        peer_id: uuid::Uuid,
        folder: bool,
        discovered: Option<SocketAddr>,
        cx: &mut Context<Self>,
    ) {
        let address = discovered.map(|mut address| {
            address.set_port(43872);
            address
        });
        let address = address.map_or_else(
            || nearby_address(self.search_input.read(cx).text(), 43872),
            Ok,
        );
        let Ok(address) = address else {
            self.notice = Some("Enter the other computer's LAN IP first".into());
            cx.notify();
            return;
        };
        self.notice = Some("Choose what to send…".into());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { send_path_with_picker(address, peer_id, folder) })
                .await;
            let _ = this.update(cx, |palette, cx| {
                palette.notice = Some(result.unwrap_or_else(|error| error));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn rebuild_emoji_rows(&mut self) {
        let entries = self.model.results().collect::<Vec<_>>();
        self.emoji_rows = build_emoji_rows(&entries, self.model.query());
        self.emoji_scroll.reset(self.emoji_rows.len());
    }

    fn reveal_selected_emoji(&self) {
        if let Some(row) = self
            .emoji_rows
            .iter()
            .position(|row| row.contains_entry(self.model.selected_index()))
        {
            self.emoji_scroll.scroll_to_reveal_item(row);
        }
    }

    fn refresh_currency(&mut self, cx: &mut Context<Self>) {
        let Some(query) = parse_currency_for_locale(self.model.query(), &self.locale.currency)
        else {
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
        self.show_currency_result();
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
        let mut entries = match &self.currency_result {
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
        if let Some(input) = self.currency_input.as_deref()
            && !input.trim().is_empty()
        {
            entries.extend(fallback_entries(input, &self.browser_name));
        }
        self.model.replace_entries_ordered(entries);
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
        let selected = self.model.selected_entry().cloned();
        let selected_index = self.model.selected_index();
        let action_mode = self.model.mode() == PaletteMode::Actions;
        if self.surface == PaletteSurface::Emoji && !action_mode {
            return emoji_view(
                selected.as_ref(),
                self.search_input.clone(),
                self.emoji_scroll.clone(),
                &self.focus,
                self.focused_text_target,
                self.emoji_category,
                self.emoji_category_open,
                colors,
                cx,
            );
        }
        let matches = self.model.results().cloned().collect::<Vec<_>>();
        if self.surface == PaletteSurface::Clipboard {
            let actions = selected
                .as_ref()
                .map_or_else(Vec::new, |entry| entry.actions.clone());
            let preview = selected.as_ref().and_then(|entry| {
                self.clipboard
                    .as_ref()
                    .and_then(|history| history.preview(&entry.id))
            });
            return clipboard_view(
                &matches,
                selected_index,
                action_mode,
                &actions,
                preview,
                self.clipboard_filter,
                self.clipboard_filter_open,
                self.notice.clone(),
                self.search_input.clone(),
                &self.results_scroll,
                &self.focus,
                colors,
                cx,
            );
        }
        if self.surface == PaletteSurface::Nearby {
            return nearby_view(
                &self.nearby_devices,
                &self.discovered_devices,
                self.local_device_id,
                self.pairing_code.as_deref(),
                self.pairing_active,
                self.notice.clone(),
                self.search_input.clone(),
                &self.focus,
                colors,
                cx,
            );
        }
        let fallback_results = !matches.is_empty()
            && matches
                .iter()
                .all(|entry| entry.id.starts_with("fallback:"));
        let calculation_with_fallbacks = !action_mode
            && matches
                .first()
                .is_some_and(|entry| entry.kind == PaletteEntryKind::Calculation)
            && matches
                .iter()
                .any(|entry| entry.id.starts_with("fallback:"));
        let section_title = if action_mode {
            selected.as_ref().map_or_else(
                || "Actions".into(),
                |entry| format!("Actions for {}", entry.title),
            )
        } else if self.surface != PaletteSurface::Launcher {
            match self.surface {
                PaletteSurface::Clipboard => "Clipboard History",
                PaletteSurface::Emoji => "Emoji & Symbols",
                PaletteSurface::Currency => "Currency & Crypto",
                PaletteSurface::Nearby => "Nearby Sharing",
                PaletteSurface::Launcher => unreachable!(),
            }
            .into()
        } else if calculation_with_fallbacks {
            matches.first().map_or_else(
                || "Calculator".into(),
                |entry| {
                    if entry.id.starts_with("currency:") {
                        "Currency Conversion".into()
                    } else if entry.id.starts_with("time:") {
                        "Time Conversion".into()
                    } else {
                        "Calculator".into()
                    }
                },
            )
        } else if fallback_results {
            format!("Use “{}” with…", self.model.query())
        } else if self.model.query().is_empty() {
            "Favorites".into()
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
            let mut rows = Vec::new();
            let mut fallback_heading_added = false;
            for (index, entry) in matches.iter().enumerate() {
                if calculation_with_fallbacks
                    && entry.id.starts_with("fallback:")
                    && !fallback_heading_added
                {
                    rows.push(
                        div()
                            .h(px(34.0))
                            .px(px(6.0))
                            .pt(px(10.0))
                            .flex()
                            .items_center()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.muted)
                            .child(format!("Use “{}” with…", self.model.query()))
                            .into_any_element(),
                    );
                    fallback_heading_added = true;
                }
                rows.push(result_row(
                    index,
                    entry,
                    index == selected_index,
                    colors,
                    cx,
                ));
            }
            rows
        };

        let footer_label = self.notice.clone().unwrap_or_else(|| {
            if self.surface != PaletteSurface::Launcher {
                match self.surface {
                    PaletteSurface::Clipboard => format!("{} clipboard items", matches.len()),
                    PaletteSurface::Emoji => format!("{} items", matches.len()),
                    PaletteSurface::Currency => "Live rates · cached for offline use".into(),
                    PaletteSurface::Nearby => {
                        format!("{} paired devices", self.nearby_devices.len())
                    }
                    PaletteSurface::Launcher => unreachable!(),
                }
            } else if self.model.query().is_empty() {
                format!("{} applications", self.application_count)
            } else {
                format!("{} results", matches.len())
            }
        });
        let primary_action = if action_mode {
            "Run".to_owned()
        } else {
            selected
                .as_ref()
                .and_then(|entry| entry.actions.first())
                .map_or_else(|| "Open".to_owned(), |action| action.title.clone())
        };

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
                div()
                    .h(px(54.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(line_icon("icons/search.svg", colors, px(18.0)))
                    .child(
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
                            .h(px(28.0))
                            .px(px(9.0))
                            .flex()
                            .items_center()
                            .gap(px(7.0))
                            .rounded(px(8.0))
                            .bg(colors.surface)
                            .text_size(px(11.0))
                            .text_color(colors.muted)
                            .child(primary_action)
                            .child(keycap("↵", colors))
                            .when(!action_mode && selected.is_some(), |footer| {
                                footer.child(keycap("⌘ K", colors))
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
            .into_any_element()
    }
}

fn build_emoji_rows(entries: &[&PaletteEntry], query: &str) -> Vec<EmojiRow> {
    if entries.is_empty() {
        return Vec::new();
    }
    if !query.trim().is_empty() {
        let mut rows = vec![EmojiRow::Header {
            label: "Search Results",
            count: entries.len(),
        }];
        rows.extend(
            (0..entries.len())
                .collect::<Vec<_>>()
                .chunks(EMOJI_COLUMNS)
                .map(|indices| EmojiRow::Tiles(indices.to_vec())),
        );
        return rows;
    }

    let sections = [
        ("emoji-group:frequent", "Frequently Used"),
        ("emoji-group:smileys-people", "Smileys & People"),
        ("emoji-group:animals-nature", "Animals & Nature"),
        ("emoji-group:food-drink", "Food & Drink"),
        ("emoji-group:travel-places", "Travel & Places"),
        ("emoji-group:activities", "Activities"),
        ("emoji-group:objects", "Objects"),
        ("emoji-group:symbols", "Symbols"),
        ("emoji-group:flags", "Flags"),
    ];
    let mut rows = Vec::new();
    for (tag, label) in sections {
        let indices = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let frequent = entry
                    .keywords
                    .iter()
                    .any(|keyword| keyword == "emoji-group:frequent");
                let matches = entry.keywords.iter().any(|keyword| keyword == tag);
                (matches && (tag == "emoji-group:frequent" || !frequent)).then_some(index)
            })
            .collect::<Vec<_>>();
        if indices.is_empty() {
            continue;
        }
        rows.push(EmojiRow::Header {
            label,
            count: indices.len(),
        });
        rows.extend(
            indices
                .chunks(EMOJI_COLUMNS)
                .map(|indices| EmojiRow::Tiles(indices.to_vec())),
        );
    }
    rows
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "emoji picker is a focused surface with explicit dependencies"
)]
fn emoji_view(
    selected: Option<&PaletteEntry>,
    search_input: Entity<SearchInput>,
    results_scroll: ListState,
    focus: &FocusHandle,
    focused_text_target: bool,
    category: EmojiCategory,
    category_open: bool,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    let selected_name =
        selected.map_or("Choose an emoji or symbol", |entry| entry.subtitle.as_str());
    let has_entries = selected.is_some();

    div()
        .id("emoji-picker")
        .relative()
        .size_full()
        .flex()
        .flex_col()
        .bg(colors.background)
        .text_color(colors.text)
        .rounded(px(18.0))
        .shadow(vec![BoxShadow {
            color: colors.shadow,
            offset: point(px(0.0), px(12.0)),
            blur_radius: px(36.0),
            spread_radius: px(-8.0),
            inset: false,
        }])
        .overflow_hidden()
        .track_focus(focus)
        .on_key_down(cx.listener(Palette::key_down))
        .child(
            div()
                .h(px(56.0))
                .px(px(14.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .border_b_1()
                .border_color(colors.divider)
                .child(
                    div()
                        .id("emoji-back")
                        .size(px(30.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(9.0))
                        .bg(colors.surface)
                        .hover(move |button| button.bg(colors.hovered))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.enter_surface(PaletteSurface::Launcher, window, cx);
                        }))
                        .child(
                            svg()
                                .path("icons/back.svg")
                                .size(px(18.0))
                                .text_color(colors.text),
                        ),
                )
                .child(div().h(px(38.0)).min_w_0().flex_1().child(search_input))
                .child(
                    div()
                        .id("emoji-category-trigger")
                        .w(px(180.0))
                        .h(px(34.0))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .rounded(px(9.0))
                        .border_1()
                        .border_color(colors.divider)
                        .bg(colors.surface)
                        .text_size(px(12.0))
                        .text_color(colors.text)
                        .hover(move |trigger| trigger.bg(colors.hovered))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.emoji_category_open = !this.emoji_category_open;
                            cx.notify();
                        }))
                        .child(category.label())
                        .child(
                            div()
                                .text_size(px(14.0))
                                .text_color(colors.muted)
                                .child("⌄"),
                        ),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .px(px(14.0))
                .pt(px(7.0))
                .pb(px(6.0))
                .when(!has_entries, |body| {
                    body.flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(12.0))
                        .text_color(colors.muted)
                        .child("No emoji or symbols match that search")
                })
                .when(has_entries, |body| {
                    body.child(
                        list(
                            results_scroll,
                            cx.processor(move |this, row_index: usize, _, cx| {
                                let Some(row) = this.emoji_rows.get(row_index).cloned() else {
                                    return div().into_any_element();
                                };
                                match row {
                                    EmojiRow::Header { label, count } => div()
                                        .h(px(38.0))
                                        .pt(px(8.0))
                                        .flex()
                                        .items_center()
                                        .gap(px(10.0))
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(label)
                                        .child(
                                            div()
                                                .font_weight(FontWeight::NORMAL)
                                                .text_color(colors.muted)
                                                .child(count.to_string()),
                                        )
                                        .into_any_element(),
                                    EmojiRow::Tiles(indices) => {
                                        let selected_index = this.model.selected_index();
                                        div()
                                            .h(px(82.0))
                                            .w_full()
                                            .pb(px(8.0))
                                            .grid()
                                            .grid_cols(8)
                                            .gap(px(8.0))
                                            .children(indices.into_iter().filter_map(|index| {
                                                let entry =
                                                    this.model.results().nth(index)?.clone();
                                                Some(emoji_tile(
                                                    index,
                                                    &entry,
                                                    index == selected_index,
                                                    colors,
                                                    cx,
                                                ))
                                            }))
                                            .into_any_element()
                                    }
                                }
                            }),
                        )
                        .size_full(),
                    )
                }),
        )
        .child(
            div()
                .h(px(40.0))
                .px(px(12.0))
                .border_t_1()
                .border_color(colors.divider)
                .flex()
                .items_center()
                .justify_between()
                .text_size(px(11.0))
                .text_color(colors.muted)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .size(px(20.0))
                                .rounded(px(6.0))
                                .bg(colors.tool_icon)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    svg()
                                        .path("icons/smile.svg")
                                        .size(px(13.0))
                                        .text_color(hsla(0.0, 0.0, 1.0, 0.96)),
                                ),
                        )
                        .child(format!("Emoji & Symbols · {selected_name}")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(if focused_text_target {
                            "Paste to active app"
                        } else {
                            "Copy to Clipboard"
                        })
                        .child(keycap("↵", colors)),
                ),
        )
        .when(category_open, |picker| {
            picker.child(
                div()
                    .id("emoji-category-menu")
                    .absolute()
                    .top(px(50.0))
                    .right(px(14.0))
                    .w(px(180.0))
                    .p(px(4.0))
                    .rounded(px(10.0))
                    .bg(colors.background)
                    .border_1()
                    .border_color(colors.divider)
                    .shadow(vec![BoxShadow {
                        color: colors.shadow,
                        offset: point(px(0.0), px(8.0)),
                        blur_radius: px(22.0),
                        spread_radius: px(-4.0),
                        inset: false,
                    }])
                    .children(
                        EmojiCategory::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, option)| {
                                div()
                                    .id(("emoji-category-option", index))
                                    .h(px(30.0))
                                    .px(px(8.0))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .rounded(px(6.0))
                                    .text_size(px(12.0))
                                    .when(option == category, |row| row.bg(colors.selected))
                                    .hover(move |row| row.bg(colors.hovered))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.emoji_category = option;
                                        this.emoji_category_open = false;
                                        this.refresh_results(cx);
                                        cx.notify();
                                    }))
                                    .child(option.label())
                                    .child(if option == category { "✓" } else { "" })
                            }),
                    ),
            )
        })
        .into_any_element()
}

fn emoji_tile(
    index: usize,
    entry: &PaletteEntry,
    selected: bool,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    div()
        .id(("emoji-tile", index))
        .h(px(74.0))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(9.0))
        .bg(colors.surface)
        .text_size(px(30.0))
        .when(selected, |tile| {
            tile.bg(colors.selected)
                .border_2()
                .border_color(colors.accent.opacity(0.78))
        })
        .hover(move |tile| tile.bg(colors.hovered))
        .on_click(cx.listener(move |this, _: &gpui::ClickEvent, window, cx| {
            let palette_event = this.model.invoke(index);
            this.handle_event(palette_event, window, cx);
            cx.notify();
        }))
        .child(entry.title.clone())
        .into_any_element()
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Nearby is a self-contained workspace with explicit state inputs"
)]
fn nearby_view(
    devices: &[TrustedDevice],
    discovered: &HashMap<uuid::Uuid, NearbyDevice>,
    local_id: Option<uuid::Uuid>,
    pairing_code: Option<&str>,
    pairing_active: bool,
    notice: Option<String>,
    search_input: Entity<SearchInput>,
    focus: &FocusHandle,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    let trusted_ids = devices
        .iter()
        .map(|device| device.id)
        .collect::<HashSet<_>>();
    let mut unpaired = discovered
        .values()
        .filter(|device| !trusted_ids.contains(&device.id))
        .cloned()
        .collect::<Vec<_>>();
    unpaired.sort_by(|left, right| left.name.cmp(&right.name));
    let discovered_rows = unpaired.into_iter().filter_map(|device| {
        let address = *device.addresses.first()?;
        Some(
            div()
                .id(device.id.to_string())
                .w_full()
                .p(px(12.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .rounded(px(12.0))
                .border_1()
                .border_color(colors.accent.opacity(0.35))
                .bg(colors.surface)
                .child(tool_icon_tile("icons/nearby.svg", colors.accent, px(16.0)))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(device.name),
                        )
                        .child(
                            div()
                                .mt(px(2.0))
                                .text_size(px(10.0))
                                .text_color(colors.muted)
                                .child(format!("Discovered automatically · {address}")),
                        ),
                )
                .child(nearby_button("Pair", colors).on_click(
                    cx.listener(move |this, _, _, cx| this.start_pairing(false, Some(address), cx)),
                )),
        )
    });
    let device_rows = devices.iter().map(|device| {
        let id = device.id;
        let toggle_id = id;
        let forget_id = id;
        let sync_id = id;
        let receive_id = id;
        let send_file_id = id;
        let send_folder_id = id;
        let receive_file_id = id;
        let discovered_address = discovered
            .get(&id)
            .and_then(|device| device.addresses.first())
            .copied();
        let sync_address = discovered_address;
        let send_file_address = discovered_address;
        let send_folder_address = discovered_address;
        div()
            .id(id.to_string())
            .w_full()
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .rounded(px(12.0))
            .bg(colors.surface)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(tool_icon_tile(
                        "icons/nearby.svg",
                        colors.tool_icon,
                        px(16.0),
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(device.name.clone()),
                            )
                            .child(
                                div()
                                    .mt(px(2.0))
                                    .text_size(px(10.0))
                                    .text_color(colors.muted)
                                    .child(if device.enabled {
                                        format!("Trusted · {}", device.id)
                                    } else {
                                        "Sharing paused".into()
                                    }),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.0))
                    .child(nearby_button("Receive clip", colors).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.start_nearby_process("clipboard", receive_id, true, None, cx);
                        },
                    )))
                    .child(nearby_button("Sync clip", colors).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.start_nearby_process(
                                "clipboard",
                                sync_id,
                                false,
                                sync_address,
                                cx,
                            );
                        },
                    )))
                    .child(nearby_button("Send file", colors).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.send_nearby_path(send_file_id, false, send_file_address, cx);
                        },
                    )))
                    .child(nearby_button("Send folder", colors).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.send_nearby_path(send_folder_id, true, send_folder_address, cx);
                        },
                    )))
                    .child(nearby_button("Receive file", colors).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.start_nearby_process("file", receive_file_id, true, None, cx);
                        },
                    )))
                    .child(
                        nearby_button(if device.enabled { "Pause" } else { "Enable" }, colors)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_nearby_device(toggle_id, cx);
                            })),
                    )
                    .child(nearby_button("Forget", colors).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.forget_nearby_device(forget_id, cx);
                        },
                    ))),
            )
    });

    div()
        .id("nearby-workspace")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(colors.background)
        .text_color(colors.text)
        .rounded(px(20.0))
        .overflow_hidden()
        .track_focus(focus)
        .on_key_down(cx.listener(Palette::key_down))
        .child(
            div()
                .h(px(58.0))
                .flex_none()
                .px(px(14.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .border_b_1()
                .border_color(colors.divider)
                .child(
                    div()
                        .id("nearby-back")
                        .size(px(32.0))
                        .rounded(px(8.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(move |button| button.bg(colors.hovered))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.enter_surface(PaletteSurface::Launcher, window, cx);
                        }))
                        .child(line_icon("icons/back.svg", colors, px(17.0))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(search_input),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .p(px(16.0))
                .flex()
                .flex_col()
                .gap(px(12.0))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .child(
                                    div()
                                        .text_size(px(15.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("Nearby Sharing"),
                                )
                                .child(
                                    div()
                                        .mt(px(3.0))
                                        .text_size(px(10.0))
                                        .text_color(colors.muted)
                                        .child(local_id.map_or_else(
                                            || "Local identity unavailable".into(),
                                            |id| format!("This device · {id}"),
                                        )),
                                ),
                        )
                        .when_some(pairing_code.map(str::to_owned), |header, code| {
                            header.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .px(px(12.0))
                                            .py(px(7.0))
                                            .rounded(px(8.0))
                                            .bg(colors.selected)
                                            .text_size(px(18.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child(code),
                                    )
                                    .child(nearby_button("Confirm", colors).on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.confirm_pairing(true, cx);
                                        },
                                    )))
                                    .child(nearby_button("Reject", colors).on_click(cx.listener(
                                        |this, _, _, cx| this.confirm_pairing(false, cx),
                                    ))),
                            )
                        }),
                )
                .child(div().text_size(px(11.0)).text_color(colors.muted).child(
                    notice.unwrap_or_else(|| {
                        if pairing_active {
                            "Pairing in progress…".into()
                        } else {
                            "Enter the other computer's IP above, or wait for it to connect.".into()
                        }
                    }),
                ))
                .child(
                    div()
                        .id("nearby-device-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .when(devices.is_empty() && discovered.is_empty(), |panel| {
                            panel.flex().items_center().justify_center().child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(colors.muted)
                                    .child("No paired computers yet"),
                            )
                        })
                        .children(discovered_rows)
                        .children(device_rows),
                ),
        )
        .child(
            div()
                .absolute()
                .bottom(px(14.0))
                .right(px(14.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    nearby_button("Pair this computer", colors).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.start_pairing(true, None, cx);
                        },
                    )),
                )
                .child(nearby_button("Connect to IP", colors).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.start_pairing(false, None, cx);
                    },
                ))),
        )
        .into_any_element()
}

fn nearby_button(label: &'static str, colors: theme::Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .flex_none()
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(colors.divider)
        .bg(colors.surface)
        .text_size(px(10.0))
        .hover(move |button| button.bg(colors.hovered))
        .child(label)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the clipboard workspace is a single declarative composition"
)]
fn clipboard_view(
    entries: &[PaletteEntry],
    selected_index: usize,
    action_mode: bool,
    actions: &[ActionItem],
    preview: Option<ClipboardPreview>,
    filter: ClipboardFilter,
    filter_open: bool,
    notice: Option<String>,
    search_input: Entity<SearchInput>,
    results_scroll: &ScrollHandle,
    focus: &FocusHandle,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    let rows = if action_mode {
        actions
            .iter()
            .enumerate()
            .map(|(index, action)| action_row(index, action, index == selected_index, colors, cx))
            .collect::<Vec<_>>()
    } else {
        entries
            .iter()
            .enumerate()
            .map(|(index, entry)| clipboard_row(index, entry, index == selected_index, colors, cx))
            .collect::<Vec<_>>()
    };
    let preview_panel = preview.map_or_else(
        || {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.0))
                .text_color(colors.muted)
                .child("Select an item to preview")
                .into_any_element()
        },
        |preview| clipboard_preview(preview, colors),
    );
    let footer_label = notice.unwrap_or_else(|| format!("{} items", entries.len()));

    div()
        .id("clipboard-workspace")
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(colors.background)
        .text_color(colors.text)
        .rounded(px(18.0))
        .shadow(vec![BoxShadow {
            color: colors.shadow,
            offset: point(px(0.0), px(12.0)),
            blur_radius: px(36.0),
            spread_radius: px(-8.0),
            inset: false,
        }])
        .overflow_hidden()
        .track_focus(focus)
        .on_key_down(cx.listener(Palette::key_down))
        .child(
            div()
                .h(px(56.0))
                .px(px(12.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .border_b_1()
                .border_color(colors.divider)
                .child(
                    div()
                        .id("clipboard-back")
                        .size(px(30.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(7.0))
                        .text_color(colors.text)
                        .hover(move |button| button.bg(colors.hovered))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.enter_surface(PaletteSurface::Launcher, window, cx);
                        }))
                        .child(
                            svg()
                                .path("icons/back.svg")
                                .size(px(18.0))
                                .text_color(colors.text),
                        ),
                )
                .child(div().h(px(38.0)).min_w_0().flex_1().child(search_input))
                .child(
                    div()
                        .id("clipboard-filter")
                        .h(px(34.0))
                        .w(px(122.0))
                        .px(px(10.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .rounded(px(8.0))
                        .bg(colors.surface)
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .hover(move |button| button.bg(colors.hovered))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clipboard_filter_open = !this.clipboard_filter_open;
                            cx.notify();
                        }))
                        .child(filter.label())
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(colors.muted)
                                .child("⌄"),
                        ),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .w(px(306.0))
                        .min_h_0()
                        .px(px(8.0))
                        .pt(px(10.0))
                        .border_r_1()
                        .border_color(colors.divider)
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(24.0))
                                .px(px(6.0))
                                .text_size(px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors.muted)
                                .child(if action_mode { "Actions" } else { "Recent" }),
                        )
                        .child(
                            div()
                                .id("clipboard-results")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .track_scroll(results_scroll)
                                .when(rows.is_empty(), |list| {
                                    list.child(
                                        div()
                                            .size_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_size(px(12.0))
                                            .text_color(colors.muted)
                                            .child("Copy something to get started"),
                                    )
                                })
                                .children(rows),
                        ),
                )
                .child(div().flex_1().min_w_0().min_h_0().child(preview_panel)),
        )
        .child(
            div()
                .h(px(38.0))
                .px(px(12.0))
                .border_t_1()
                .border_color(colors.divider)
                .flex()
                .items_center()
                .justify_between()
                .text_size(px(11.0))
                .text_color(colors.muted)
                .child(div().flex().items_center().child("Clipboard History"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(footer_label)
                        .child(if action_mode { "Run" } else { "Copy" })
                        .child(keycap("↵", colors))
                        .child("Actions")
                        .child(keycap("⌘ K", colors)),
                ),
        )
        .when(filter_open, |workspace| {
            workspace.child(
                div()
                    .id("clipboard-filter-menu")
                    .absolute()
                    .top(px(50.0))
                    .right(px(12.0))
                    .w(px(122.0))
                    .p(px(4.0))
                    .rounded(px(9.0))
                    .bg(colors.background)
                    .border_1()
                    .border_color(colors.divider)
                    .shadow(vec![BoxShadow {
                        color: colors.shadow,
                        offset: point(px(0.0), px(8.0)),
                        blur_radius: px(20.0),
                        spread_radius: px(-4.0),
                        inset: false,
                    }])
                    .children(ClipboardFilter::ALL.into_iter().enumerate().map(
                        |(index, option)| {
                            div()
                                .id(("clipboard-filter-option", index))
                                .h(px(30.0))
                                .px(px(8.0))
                                .flex()
                                .items_center()
                                .justify_between()
                                .rounded(px(6.0))
                                .text_size(px(12.0))
                                .when(option == filter, |row| row.bg(colors.selected))
                                .hover(move |row| row.bg(colors.hovered))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.clipboard_filter = option;
                                    this.clipboard_filter_open = false;
                                    this.refresh_results(cx);
                                    cx.notify();
                                }))
                                .child(option.label())
                                .child(if option == filter { "✓" } else { "" })
                        },
                    )),
            )
        })
        .into_any_element()
}

fn clipboard_row(
    index: usize,
    entry: &PaletteEntry,
    selected: bool,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    let entry = entry.clone();
    div()
        .id(("clipboard-row", index))
        .h(px(48.0))
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
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(entry.title),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(10.0))
                        .text_color(colors.muted)
                        .child(entry.subtitle),
                ),
        )
        .when(entry.favorite, |row| {
            row.child(
                div()
                    .text_size(px(9.0))
                    .text_color(colors.muted)
                    .child("PINNED"),
            )
        })
        .into_any_element()
}

fn clipboard_preview(preview: ClipboardPreview, colors: theme::Theme) -> AnyElement {
    let pin = if preview.pinned { " · Pinned" } else { "" };
    let metadata = format!(
        "{} · {} characters · {} words · {}{}",
        preview.content_type, preview.characters, preview.words, preview.age, pin
    );
    div()
        .size_full()
        .min_h_0()
        .flex()
        .flex_col()
        .child(
            div()
                .id("clipboard-preview-content")
                .flex_1()
                .min_h_0()
                .px(px(18.0))
                .py(px(16.0))
                .overflow_y_scroll()
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.muted)
                        .mb(px(10.0))
                        .child(preview.title),
                )
                .child(
                    div()
                        .whitespace_normal()
                        .text_size(px(13.0))
                        .line_height(px(20.0))
                        .child(preview.body),
                ),
        )
        .child(
            div()
                .h(px(56.0))
                .px(px(18.0))
                .bg(colors.surface)
                .border_t_1()
                .border_color(colors.divider)
                .flex()
                .flex_col()
                .justify_center()
                .gap(px(3.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(FontWeight::MEDIUM)
                        .child(preview.source),
                )
                .child(
                    div()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_size(px(10.0))
                        .text_color(colors.muted)
                        .child(metadata),
                ),
        )
        .into_any_element()
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

#[allow(
    clippy::too_many_lines,
    reason = "the two-column calculation card is clearest as one declarative tree"
)]
fn calculation_row(
    index: usize,
    entry: PaletteEntry,
    selected: bool,
    colors: theme::Theme,
    cx: &mut Context<Palette>,
) -> AnyElement {
    if !entry.actions.is_empty() {
        return div()
            .id(("calculation-row", index))
            .h(px(104.0))
            .mx(px(2.0))
            .mb(px(4.0))
            .flex()
            .items_stretch()
            .overflow_hidden()
            .rounded(px(10.0))
            .bg(if selected {
                colors.selected
            } else {
                colors.surface
            })
            .hover(move |card| card.bg(colors.hovered))
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
                    .px(px(22.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_center()
                            .text_size(px(19.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(entry.subtitle),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.muted)
                            .child("INPUT"),
                    ),
            )
            .child(
                div()
                    .w(px(48.0))
                    .h_full()
                    .flex_shrink_0()
                    .border_l_1()
                    .border_r_1()
                    .border_color(colors.divider)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(18.0))
                    .text_color(colors.muted)
                    .child("→"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(22.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(7.0))
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_center()
                            .text_size(px(21.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(entry.title),
                    )
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_center()
                            .text_size(px(10.0))
                            .text_color(colors.muted)
                            .child(entry.preview),
                    ),
            )
            .into_any_element();
    }

    div()
        .id(("calculation-row", index))
        .h(px(72.0))
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(12.0))
        .rounded(px(10.0))
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
                        .w(px(210.0))
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(15.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child(entry.subtitle),
                        )
                        .child(
                            div()
                                .text_size(px(9.0))
                                .text_color(colors.muted)
                                .child("INPUT"),
                        ),
                )
                .child(
                    div()
                        .text_size(px(16.0))
                        .text_color(colors.muted)
                        .child("→"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(17.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(entry.title),
                        )
                        .child(
                            div()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_size(px(9.0))
                                .text_color(colors.muted)
                                .child(entry.preview),
                        ),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(11.0))
                .text_color(colors.muted)
                .child("Keep typing"),
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
    if let Some(path) = entry.icon.as_ref().filter(|path| is_renderable_image(path)) {
        let fallback_label = entry
            .title
            .chars()
            .next()
            .unwrap_or('A')
            .to_uppercase()
            .to_string();
        return img(path.clone())
            .size(px(24.0))
            .rounded(px(5.0))
            .with_fallback(move || fallback_icon(fallback_label.clone(), colors))
            .into_any_element();
    }
    if entry.kind == PaletteEntryKind::Application {
        return fallback_icon(
            entry
                .title
                .chars()
                .next()
                .unwrap_or('A')
                .to_uppercase()
                .to_string(),
            colors,
        );
    }
    if entry.kind == PaletteEntryKind::Emoji {
        return div()
            .w(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(17.0))
            .child(entry.title.clone())
            .into_any_element();
    }
    if entry.id == "fallback:web" {
        return brand_icon("icons/google.svg", px(20.0), hsla(0.60, 0.75, 0.56, 1.0));
    }
    if entry.id == "fallback:files" {
        return brand_icon("icons/finder.svg", px(22.0), hsla(0.56, 0.78, 0.56, 1.0));
    }
    let path = match entry.id.as_str() {
        "builtin:clipboard" => "icons/clipboard.svg",
        "builtin:nearby" => "icons/nearby.svg",
        "tool:currency" | "currency:loading" | "currency:result" | "intent:currency" => {
            "icons/coins.svg"
        }
        "tool:emoji" => "icons/smile.svg",
        "tool:uuid" => "icons/hash.svg",
        "tool:timestamp" | "intent:time" | "time:result" => "icons/clock.svg",
        "intent:calculation" => "icons/calculator.svg",
        _ => match entry.kind {
            PaletteEntryKind::File => "icons/file.svg",
            PaletteEntryKind::Clipboard => "icons/clipboard.svg",
            PaletteEntryKind::Calculation => "icons/calculator.svg",
            PaletteEntryKind::Command | PaletteEntryKind::Tool => "icons/command.svg",
            PaletteEntryKind::Application | PaletteEntryKind::Emoji => unreachable!(),
        },
    };
    if matches!(
        entry.kind,
        PaletteEntryKind::Tool | PaletteEntryKind::Command | PaletteEntryKind::Calculation
    ) || entry.id == "builtin:clipboard"
    {
        tool_icon_tile(path, colors.tool_icon, px(16.0))
    } else {
        line_icon(path, colors, px(18.0))
    }
}

fn tool_icon_tile(
    path: &'static str,
    background: gpui::Hsla,
    icon_size: gpui::Pixels,
) -> AnyElement {
    div()
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .bg(linear_gradient(
            135.0,
            linear_color_stop(background, 0.0),
            linear_color_stop(hsla(0.77, 0.66, 0.35, 1.0), 1.0),
        ))
        .child(svg().path(path).size(icon_size).text_color(gpui::white()))
        .into_any_element()
}

fn brand_icon(path: &'static str, icon_size: gpui::Pixels, color: gpui::Hsla) -> AnyElement {
    div()
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(path).size(icon_size).text_color(color))
        .into_any_element()
}

fn line_icon(path: &'static str, colors: theme::Theme, icon_size: gpui::Pixels) -> AnyElement {
    div()
        .w(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .child(svg().path(path).size(icon_size).text_color(colors.muted))
        .into_any_element()
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
        title: format!("Converting to {}…", query.to),
        subtitle: format!("{} {}", query.amount, query.from),
        kind: PaletteEntryKind::Calculation,
        icon: None,
        keywords: vec![input.to_owned()],
        preview: "Fetching the latest rate".into(),
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
        subtitle: format!("{} {}", result.query.amount, result.query.from),
        kind: PaletteEntryKind::Calculation,
        icon: None,
        keywords: vec![
            input.to_owned(),
            result.query.from.to_string(),
            result.query.to.to_string(),
        ],
        preview: observed,
        frequency: u32::MAX,
        favorite: true,
        actions: vec![ActionItem {
            id: "copy-currency".into(),
            title: "Copy Converted Amount".into(),
            shortcut: Some("↵".into()),
        }],
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolIntent {
    Calculation,
    Currency,
    Time,
}

fn tool_intent(query: &str) -> Option<ToolIntent> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() || !query.chars().next()?.is_ascii_digit() {
        return None;
    }
    if query.contains("am") || query.contains("pm") {
        return Some(ToolIntent::Time);
    }
    if query
        .chars()
        .last()
        .is_some_and(|character| matches!(character, '+' | '-' | '*' | '/' | '%' | '^'))
    {
        return Some(ToolIntent::Calculation);
    }
    let suffix = query
        .trim_start_matches(|character: char| character.is_ascii_digit() || character == '.')
        .trim();
    (!suffix.is_empty() && CURRENCY_PREFIXES.contains(&suffix)).then_some(ToolIntent::Currency)
}

fn intent_entry(intent: ToolIntent, query: &str) -> PaletteEntry {
    let (id, title, preview) = match intent {
        ToolIntent::Calculation => (
            "intent:calculation",
            "Finish the calculation…",
            "Calculator",
        ),
        ToolIntent::Currency => (
            "intent:currency",
            "Finish the currency code…",
            "Currency converter",
        ),
        ToolIntent::Time => ("intent:time", "Finish the time zone…", "Time converter"),
    };
    PaletteEntry {
        id: id.into(),
        title: title.into(),
        subtitle: query.into(),
        kind: PaletteEntryKind::Calculation,
        icon: None,
        keywords: vec![query.into()],
        preview: preview.into(),
        frequency: u32::MAX,
        favorite: true,
        actions: Vec::new(),
    }
}

fn parse_currency_for_locale(input: &str, local_currency: &str) -> Option<CurrencyQuery> {
    CurrencyQuery::parse(input)
        .or_else(|| CurrencyQuery::parse_with_default(input, local_currency))
        .or_else(|| {
            let fallback = if local_currency.eq_ignore_ascii_case("USD") {
                "EUR"
            } else {
                "USD"
            };
            CurrencyQuery::parse_with_default(input, fallback)
        })
}

const fn empty_title(surface: PaletteSurface) -> &'static str {
    match surface {
        PaletteSurface::Currency => "Type a conversion",
        PaletteSurface::Emoji => "No emoji or symbol found",
        PaletteSurface::Clipboard => "No clipboard items",
        PaletteSurface::Nearby => "No paired devices",
        PaletteSurface::Launcher => "No matches found",
    }
}

const fn empty_hint(surface: PaletteSurface) -> &'static str {
    match surface {
        PaletteSurface::Currency => "Example: 100 USD to EUR",
        PaletteSurface::Emoji => "Try a feeling, object, or symbol",
        PaletteSurface::Clipboard => "Copy something and it will appear here",
        PaletteSurface::Nearby => "Pair a Mac or Linux computer on your local network",
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

fn nearby_address(input: &str, default_port: u16) -> Result<SocketAddr, std::net::AddrParseError> {
    let input = input.trim();
    if input.contains(':') {
        input.parse()
    } else {
        format!("{input}:{default_port}").parse()
    }
}

fn platform_device_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Superspace Mac"
    } else {
        "Superspace Linux"
    }
}

fn discovery_host_label(id: uuid::Uuid) -> String {
    format!("superspace-{}", &id.simple().to_string()[..12])
}

fn discovery_id_from_fullname(fullname: &str) -> Option<uuid::Uuid> {
    fullname.split('.').next()?.parse().ok()
}

fn run_ui_pairing(
    root: &Path,
    listen: bool,
    address: SocketAddr,
    events: &mpsc::Sender<PairingEvent>,
) -> Result<String, String> {
    let identity =
        superspace_network::LocalIdentity::load_or_create(root.join("local-identity.cbor"))
            .map_err(|error| error.to_string())?;
    let info = superspace_network::PairingPublicInfo::for_local(&identity, "Superspace");
    let runtime = tokio::runtime::Runtime::new().map_err(|error| error.to_string())?;
    let event_sender = events.clone();
    let peer = runtime
        .block_on(async {
            let pairing = async {
                if listen {
                    let listener = tokio::net::TcpListener::bind(address).await?;
                    let (mut stream, _) = listener.accept().await?;
                    superspace_network::pair_incoming(&mut stream, &identity, &info, move |code| {
                        pairing_confirmation(code, event_sender)
                    })
                    .await
                    .map_err(anyhow::Error::from)
                } else {
                    let mut stream = tokio::net::TcpStream::connect(address).await?;
                    superspace_network::pair_outgoing(&mut stream, &identity, &info, move |code| {
                        pairing_confirmation(code, event_sender)
                    })
                    .await
                    .map_err(anyhow::Error::from)
                }
            };
            tokio::time::timeout(Duration::from_secs(5 * 60), pairing)
                .await
                .map_err(|_| anyhow::anyhow!("Pairing timed out"))?
        })
        .map_err(|error| format!("Pairing failed: {error}"))?;
    let peer_name = peer.info.name.clone();
    TrustedDeviceStore::open(root.join("trusted-devices.sqlite"))
        .and_then(|store| {
            store.upsert(&TrustedDevice {
                id: peer.info.device_id,
                name: peer.info.name,
                noise_public_key: peer.noise_public_key,
                certificate_der: peer.info.certificate_der,
                paired_at: chrono::Utc::now().timestamp_millis(),
                last_seen_at: None,
                enabled: true,
            })
        })
        .map_err(|error| format!("Could not save paired device: {error}"))?;
    Ok(format!("Paired with {peer_name}"))
}

async fn pairing_confirmation(
    code: superspace_network::PairingCode,
    events: mpsc::Sender<PairingEvent>,
) -> bool {
    let (confirmation, decision) = mpsc::channel();
    if events.send(PairingEvent::Code(code, confirmation)).is_err() {
        return false;
    }
    tokio::task::spawn_blocking(move || decision.recv().unwrap_or(false))
        .await
        .unwrap_or(false)
}

fn send_path_with_picker(
    address: SocketAddr,
    peer_id: uuid::Uuid,
    folder: bool,
) -> Result<String, String> {
    #[cfg(target_os = "linux")]
    let output = {
        let mut command = Command::new("zenity");
        command.arg("--file-selection");
        if folder {
            command.arg("--directory");
        }
        command.output()
    };
    #[cfg(target_os = "macos")]
    let output = Command::new("osascript")
        .args(if folder {
            [
                "-e",
                "POSIX path of (choose folder with prompt \"Choose a folder to send\")",
            ]
        } else {
            [
                "-e",
                "POSIX path of (choose file with prompt \"Choose a file to send\")",
            ]
        })
        .output();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err("File picking is supported only on Linux and macOS".into());
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let output = output.map_err(|error| format!("Could not open the file picker: {error}"))?;
    if !output.status.success() {
        return Ok("Sharing cancelled".into());
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|_| "The selected path is not valid UTF-8".to_owned())?;
    let path = path.trim();
    if path.is_empty() {
        return Ok("Sharing cancelled".into());
    }
    let status = Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
        .args([
            "nearby",
            "file-send",
            &address.to_string(),
            &peer_id.to_string(),
            path,
            "Superspace",
        ])
        .status()
        .map_err(|error| format!("Could not start the transfer: {error}"))?;
    if status.success() {
        Ok(format!("Sent {}", Path::new(path).display()))
    } else {
        Err("The file transfer failed; make sure the receiver is ready".into())
    }
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

fn fallback_entries(query: &str, browser_name: &str) -> Vec<PaletteEntry> {
    [
        (
            "fallback:web",
            format!("Search Google for “{query}”"),
            format!("Open in {browser_name}"),
            "search-web",
            "Google Search",
        ),
        (
            "fallback:files",
            format!("Search files for “{query}”"),
            "Open in Finder".to_owned(),
            "search-files",
            "File Search",
        ),
    ]
    .into_iter()
    .map(|(id, title, subtitle, action, preview)| PaletteEntry {
        id: id.into(),
        title,
        subtitle,
        kind: PaletteEntryKind::Command,
        icon: None,
        keywords: vec![query.into(), preview.into()],
        preview: preview.into(),
        frequency: 0,
        favorite: false,
        actions: vec![ActionItem {
            id: action.into(),
            title: preview.into(),
            shortcut: Some("↵".into()),
        }],
    })
    .collect()
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

    use super::{
        ClipboardFilter, ToolIntent, currency_loading_entry, currency_result_entry,
        fallback_entries, parse_currency_for_locale, tool_intent,
    };
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

    #[test]
    fn clipboard_filter_keyboard_order_wraps_in_both_directions() {
        assert_eq!(ClipboardFilter::All.next(), ClipboardFilter::Text);
        assert_eq!(ClipboardFilter::All.previous(), ClipboardFilter::Files);
        assert_eq!(ClipboardFilter::Files.next(), ClipboardFilter::All);
    }

    #[test]
    fn unmatched_queries_offer_real_web_and_file_actions() {
        let entries = fallback_entries("one character", "Aside");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].actions[0].id, "search-web");
        assert_eq!(entries[0].subtitle, "Open in Aside");
        assert_eq!(entries[1].actions[0].id, "search-files");
        assert!(entries.iter().all(|entry| {
            entry
                .keywords
                .iter()
                .any(|keyword| keyword == "one character")
        }));
    }

    #[test]
    fn shorthand_tools_activate_before_generic_fallbacks() {
        let currency = parse_currency_for_locale("1usd", "INR").expect("currency");
        assert_eq!(currency.to.as_str(), "INR");
        assert_eq!(tool_intent("5 +"), Some(ToolIntent::Calculation));
        assert_eq!(tool_intent("1am p"), Some(ToolIntent::Time));
        assert_eq!(tool_intent("1us"), Some(ToolIntent::Currency));
    }
}
