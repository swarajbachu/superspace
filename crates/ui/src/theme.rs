use gpui::{App, Global, Hsla, hsla};

/// Built-in appearance choices, ordered for keyboard cycling.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ThemeKind {
    /// Comet-inspired deep indigo.
    #[default]
    Nebula,
    /// Neutral near-black surface.
    Eclipse,
    /// Warm, high-contrast light surface.
    Dawn,
}

impl ThemeKind {
    /// Advance to the next built-in theme.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Nebula => Self::Eclipse,
            Self::Eclipse => Self::Dawn,
            Self::Dawn => Self::Nebula,
        }
    }

    /// Stable display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Nebula => "Nebula",
            Self::Eclipse => "Eclipse",
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
    cx.set_global(for_kind(ThemeKind::Nebula));
}

/// Resolve semantic colors for a built-in theme.
#[must_use]
pub fn for_kind(kind: ThemeKind) -> Theme {
    match kind {
        ThemeKind::Nebula => Theme {
            background: hsla(0.67, 0.28, 0.055, 0.97),
            surface: hsla(0.66, 0.22, 0.09, 0.92),
            selected: hsla(0.64, 0.28, 0.16, 0.95),
            text: hsla(0.64, 0.08, 0.94, 1.0),
            muted: hsla(0.64, 0.08, 0.65, 1.0),
            border: hsla(0.64, 0.14, 0.24, 0.7),
            accent: hsla(0.72, 0.78, 0.68, 1.0),
        },
        ThemeKind::Eclipse => Theme {
            background: hsla(0.0, 0.0, 0.035, 0.98),
            surface: hsla(0.0, 0.0, 0.075, 0.96),
            selected: hsla(0.56, 0.34, 0.18, 1.0),
            text: hsla(0.0, 0.0, 0.96, 1.0),
            muted: hsla(0.0, 0.0, 0.62, 1.0),
            border: hsla(0.0, 0.0, 0.20, 0.8),
            accent: hsla(0.54, 0.88, 0.66, 1.0),
        },
        ThemeKind::Dawn => Theme {
            background: hsla(0.10, 0.20, 0.96, 0.98),
            surface: hsla(0.10, 0.12, 0.91, 0.98),
            selected: hsla(0.71, 0.34, 0.82, 1.0),
            text: hsla(0.66, 0.22, 0.15, 1.0),
            muted: hsla(0.66, 0.10, 0.42, 1.0),
            border: hsla(0.66, 0.12, 0.76, 0.9),
            accent: hsla(0.72, 0.68, 0.47, 1.0),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_cycle_is_stable_and_complete() {
        let first = ThemeKind::default();
        assert_eq!(first.name(), "Nebula");
        assert_eq!(first.next().name(), "Eclipse");
        assert_eq!(first.next().next().name(), "Dawn");
        assert_eq!(first.next().next().next(), first);
    }
}
