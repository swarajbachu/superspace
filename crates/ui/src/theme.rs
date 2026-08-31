use gpui::{App, Global, Hsla, hsla};

/// Built-in appearance choices, ordered for keyboard cycling.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ThemeKind {
    /// Neutral translucent graphite.
    #[default]
    Graphite,
    /// Deeper near-black surface.
    Midnight,
    /// Warm, high-contrast light surface.
    Dawn,
}

impl ThemeKind {
    /// Advance to the next built-in theme.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Graphite => Self::Midnight,
            Self::Midnight => Self::Dawn,
            Self::Dawn => Self::Graphite,
        }
    }

    /// Stable display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Graphite => "Graphite",
            Self::Midnight => "Midnight",
            Self::Dawn => "Dawn",
        }
    }
}

/// Semantic paint tokens shared by every Superspace surface.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Window background.
    pub background: Hsla,
    /// Elevated row and panel background.
    pub surface: Hsla,
    /// Hovered/selected surface.
    pub selected: Hsla,
    /// Pointer hover without keyboard selection.
    pub hovered: Hsla,
    /// Primary text.
    pub text: Hsla,
    /// Secondary text.
    pub muted: Hsla,
    /// Hairline separator.
    pub border: Hsla,
    /// Interactive accent.
    pub accent: Hsla,
    /// Low-emphasis icon tile.
    pub tile: Hsla,
}

impl Global for Theme {}

/// Install the default graphite theme.
pub fn install(cx: &mut App) {
    cx.set_global(for_kind(ThemeKind::Graphite));
}

/// Resolve semantic colors for a built-in theme.
#[must_use]
pub fn for_kind(kind: ThemeKind) -> Theme {
    match kind {
        ThemeKind::Graphite => Theme {
            background: hsla(0.64, 0.08, 0.075, 0.90),
            surface: hsla(0.64, 0.06, 0.105, 0.78),
            selected: hsla(0.64, 0.07, 0.21, 0.92),
            hovered: hsla(0.64, 0.06, 0.16, 0.75),
            text: hsla(0.0, 0.0, 0.96, 1.0),
            muted: hsla(0.64, 0.03, 0.62, 1.0),
            border: hsla(0.64, 0.05, 0.30, 0.55),
            accent: hsla(0.58, 0.72, 0.68, 1.0),
            tile: hsla(0.64, 0.06, 0.18, 0.82),
        },
        ThemeKind::Midnight => Theme {
            background: hsla(0.0, 0.0, 0.025, 0.94),
            surface: hsla(0.0, 0.0, 0.065, 0.82),
            selected: hsla(0.0, 0.0, 0.18, 0.96),
            hovered: hsla(0.0, 0.0, 0.12, 0.80),
            text: hsla(0.0, 0.0, 0.96, 1.0),
            muted: hsla(0.0, 0.0, 0.62, 1.0),
            border: hsla(0.0, 0.0, 0.24, 0.62),
            accent: hsla(0.56, 0.72, 0.68, 1.0),
            tile: hsla(0.0, 0.0, 0.15, 0.90),
        },
        ThemeKind::Dawn => Theme {
            background: hsla(0.10, 0.10, 0.96, 0.93),
            surface: hsla(0.10, 0.08, 0.91, 0.82),
            selected: hsla(0.60, 0.20, 0.84, 0.95),
            hovered: hsla(0.10, 0.08, 0.88, 0.86),
            text: hsla(0.66, 0.12, 0.14, 1.0),
            muted: hsla(0.66, 0.06, 0.42, 1.0),
            border: hsla(0.66, 0.08, 0.70, 0.72),
            accent: hsla(0.60, 0.58, 0.46, 1.0),
            tile: hsla(0.10, 0.08, 0.86, 0.92),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_cycle_is_stable_and_complete() {
        let first = ThemeKind::default();
        assert_eq!(first.name(), "Graphite");
        assert_eq!(first.next().name(), "Midnight");
        assert_eq!(first.next().next().name(), "Dawn");
        assert_eq!(first.next().next().next(), first);
    }
}
