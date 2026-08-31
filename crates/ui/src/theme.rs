use gpui::{App, Global, Hsla, hsla};

/// Semantic paint tokens shared by every Superspace surface.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Window background.
    pub background: Hsla,
    /// Elevated row and panel background.
    pub surface: Hsla,
    /// Hovered/selected surface.
    pub selected: Hsla,
    /// Primary text.
    pub text: Hsla,
    /// Secondary text.
    pub muted: Hsla,
    /// Hairline separator.
    pub border: Hsla,
    /// Interactive accent.
    pub accent: Hsla,
}

impl Global for Theme {}

/// Install the default deep-indigo theme.
pub fn install(cx: &mut App) {
    cx.set_global(Theme {
        background: hsla(0.67, 0.28, 0.055, 0.97),
        surface: hsla(0.66, 0.22, 0.09, 0.92),
        selected: hsla(0.64, 0.28, 0.16, 0.95),
        text: hsla(0.64, 0.08, 0.94, 1.0),
        muted: hsla(0.64, 0.08, 0.65, 1.0),
        border: hsla(0.64, 0.14, 0.24, 0.7),
        accent: hsla(0.72, 0.78, 0.68, 1.0),
    });
}

/// Resolve the current theme.
pub fn get(cx: &App) -> Theme {
    *cx.global::<Theme>()
}
