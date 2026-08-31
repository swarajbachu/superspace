use gpui::{
    AnimationExt as _, App, AppContext as _, Context, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div, px,
};
use superspace_core::{SearchCandidate, builtin_features, rank_candidates};

use crate::{motion, theme};

/// Main command palette state.
pub struct Palette {
    query: String,
    selected: usize,
}

impl Palette {
    pub(crate) fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
        }
    }

    fn matches(&self) -> Vec<superspace_core::SearchMatch<'static>> {
        rank_candidates(
            &self.query,
            builtin_features().iter().map(|feature| SearchCandidate {
                id: feature.id,
                title: feature.title,
                keywords: &[],
                frequency: 0,
                favorite: matches!(
                    feature.id,
                    "app-launcher" | "clipboard-history" | "nearby-share"
                ),
            }),
        )
    }
}

impl Render for Palette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme::get(cx);
        let matches = self.matches();
        self.selected = self.selected.min(matches.len().saturating_sub(1));

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
            .child(
                div()
                    .h(px(68.0))
                    .px(px(22.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border)
                    .gap(px(12.0))
                    .child(div().text_size(px(20.0)).text_color(theme.accent).child("✦"))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(20.0))
                            .child(if self.query.is_empty() {
                                "Search Superspace…".to_string()
                            } else {
                                self.query.clone()
                            }),
                    )
                    .child(div().text_size(px(12.0)).text_color(theme.muted).child("⌘ Space")),
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
                            .children(matches.iter().take(10).enumerate().map(|(index, entry)| {
                                let selected = index == self.selected;
                                div()
                                    .id(("command-row", index))
                                    .h(px(42.0))
                                    .px(px(12.0))
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .rounded(px(9.0))
                                    .when(selected, |row| row.bg(theme.selected))
                                    .child(div().text_size(px(14.0)).child(entry.candidate.title))
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(theme.muted)
                                            .child(entry.candidate.id),
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
                            .child(
                                div()
                                    .text_size(px(22.0))
                                    .child(matches.first().map_or("No results", |entry| entry.candidate.title)),
                            )
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .text_color(theme.muted)
                                    .child("Launch apps, calculate, search, or send anything to a nearby device."),
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
                    .child("Nearby devices appear here automatically")
                    .child("Open ↵    Actions ⌘K"),
            )
            .with_animation("palette-enter", motion::ENTER.animation(), |element, progress| {
                element.opacity(progress).relative().top(px(6.0 * (1.0 - progress)))
            })
    }
}
