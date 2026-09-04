//! GPUI application shell for Superspace.

#[cfg(feature = "desktop")]
mod clipboard_history;
#[cfg(feature = "desktop")]
mod currency;
#[cfg(feature = "desktop")]
mod icons;
#[cfg(feature = "desktop")]
mod mini_tools;
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
    App, AppContext as _, Bounds, WindowBackgroundAppearance, WindowBounds, WindowDecorations,
    WindowKind, WindowOptions, px, size,
};

/// Start the headed Superspace application.
///
/// # Panics
/// Panics if GPUI cannot create the palette window after application startup.
#[cfg(feature = "desktop")]
pub fn run() {
    gpui_platform::application()
        .with_assets(icons::Icons)
        .run(|cx: &mut App| {
            theme::install(cx);
            search_input::init(cx);
            let focused_text_target = superspace_platform::focused_text_target();
            let bounds = Bounds::centered(None, size(px(800.0), px(500.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(620.0), px(500.0))),
                    titlebar: None,
                    window_decorations: Some(WindowDecorations::Client),
                    window_background: platform_window_background(),
                    kind: platform_window_kind(),
                    is_resizable: false,
                    is_minimizable: false,
                    ..WindowOptions::default()
                },
                |window, cx| {
                    #[cfg(target_os = "linux")]
                    window.activate_window();
                    cx.new(|cx| shell::Palette::new(window, cx, focused_text_target))
                },
            )
            .expect("open Superspace palette");
            cx.activate(true);
        });
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
fn platform_window_background() -> WindowBackgroundAppearance {
    // GPUI's X11 backend has no backdrop-blur protocol, so alpha would expose a sharp, distracting
    // desktop beneath the palette. Use a fully painted glass tint there; Wayland can request its
    // compositor blur protocol when available.
    if std::env::var("XDG_SESSION_TYPE").is_ok_and(|session| session.eq_ignore_ascii_case("x11")) {
        WindowBackgroundAppearance::Opaque
    } else {
        WindowBackgroundAppearance::Blurred
    }
}

#[cfg(all(feature = "desktop", not(target_os = "linux")))]
const fn platform_window_background() -> WindowBackgroundAppearance {
    WindowBackgroundAppearance::Blurred
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
const fn platform_window_kind() -> WindowKind {
    // PopUp is override-redirect on X11, which prevents GNOME from reliably assigning keyboard
    // focus. A borderless Normal window remains focusable while preserving the palette shape.
    WindowKind::Normal
}

#[cfg(all(feature = "desktop", not(target_os = "linux")))]
const fn platform_window_kind() -> WindowKind {
    WindowKind::PopUp
}

/// Explain how to enable the headed build when compiled without native dependencies.
#[cfg(not(feature = "desktop"))]
pub fn run() {
    eprintln!(
        "Superspace was built without its GPUI desktop backend; rebuild with `--features desktop`"
    );
}
