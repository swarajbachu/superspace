//! GPUI application shell for Superspace.

#[cfg(feature = "desktop")]
mod clipboard_history;
mod model;
#[cfg(feature = "desktop")]
pub mod motion;
#[cfg(feature = "desktop")]
mod search_input;
#[cfg(feature = "desktop")]
mod shell;
#[cfg(feature = "desktop")]
mod theme;

pub use model::{
    ActionItem, PaletteEntry, PaletteEntryKind, PaletteEvent, PaletteKey, PaletteMode, PaletteModel,
};

#[cfg(feature = "desktop")]
use gpui::{
    App, AppContext as _, Bounds, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions, px, size,
};

/// Start the headed Superspace application.
///
/// # Panics
/// Panics if GPUI cannot create the palette window after application startup.
#[cfg(feature = "desktop")]
pub fn run() {
    gpui_platform::application().run(|cx: &mut App| {
        theme::install(cx);
        search_input::init(cx);
        let bounds = Bounds::centered(None, size(px(680.0), px(420.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(620.0), px(380.0))),
                titlebar: None,
                window_background: WindowBackgroundAppearance::Blurred,
                kind: WindowKind::PopUp,
                is_resizable: false,
                is_minimizable: false,
                ..WindowOptions::default()
            },
            |window, cx| cx.new(|cx| shell::Palette::new(window, cx)),
        )
        .expect("open Superspace palette");
        cx.activate(true);
    });
}

/// Explain how to enable the headed build when compiled without native dependencies.
#[cfg(not(feature = "desktop"))]
pub fn run() {
    eprintln!(
        "Superspace was built without its GPUI desktop backend; rebuild with `--features desktop`"
    );
}
