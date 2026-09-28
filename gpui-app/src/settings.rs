//! The Settings route: the choices that belong to the app rather than to the
//! schema in front of you.
//!
//! The view switches — descriptions, bundling, primitive fields, Relay — are
//! deliberately not here. Those belong to the graph you are reading, and they
//! live on its toolbar so you can watch what they do to it as you flip them.
//! What is left is what should still be true the next time you open a
//! different file.

use crate::icons::{icon, Icon};
use crate::theme::{Theme, ThemeMode};
use gpui::{div, prelude::*, px, MouseButton, SharedString, Window};

/// The three theme choices, laid out side by side rather than hidden behind a
/// button that cycles: a settings page should show what the alternatives are.
const MODES: &[(ThemeMode, Icon, &str, &str)] = &[
    (ThemeMode::Light, Icon::Sun, "Light", "Always light."),
    (ThemeMode::Dark, Icon::Moon, "Dark", "Always dark."),
    (ThemeMode::System, Icon::Monitor, "System", "Follow macOS."),
];

fn section_title(th: Theme, text: &'static str) -> impl IntoElement {
    div()
        .mt_8()
        .mb_3()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(th.text_faint)
        .child(SharedString::from(text.to_uppercase()))
}

pub fn view<T: 'static>(
    th: Theme,
    mode: ThemeMode,
    settings_file: Option<SharedString>,
    on_theme: impl Fn(&mut T, ThemeMode, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let mut choices = div().flex().gap_3();
    for &(m, ic, label, blurb) in MODES {
        let active = m == mode;
        let pick = on_theme.clone();
        choices = choices.child(
            div()
                .id(label)
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .px_4()
                .py_3()
                .rounded_lg()
                .border_1()
                .cursor_pointer()
                .when(active, |el| {
                    el.border_color(th.primary).bg(th.active_bg).text_color(th.text)
                })
                .when(!active, |el| {
                    el.border_color(th.card_border)
                        .text_color(th.text_muted)
                        .hover(|el| el.bg(th.hover_bg).text_color(th.text))
                })
                .on_click(cx.listener(move |this, _, window, cx| pick(this, m, window, cx)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(ic, px(16.0), if active { th.text } else { th.text_muted }))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(SharedString::from(label)),
                        ),
                )
                .child(div().text_xs().text_color(th.text_faint).child(SharedString::from(blurb))),
        );
    }

    div()
        .id("settings-scroll")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(680.0))
                .mx_auto()
                .px_6()
                .py_10()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_3xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(th.text)
                        .child("Settings"),
                )
                .child(
                    div()
                        .mt_2()
                        .text_sm()
                        .line_height(px(22.0))
                        .text_color(th.text_muted)
                        .child("Saved as you change them, and applied the next time you open a schema."),
                )
                .child(section_title(th, "Appearance"))
                .child(
                    div()
                        .mb_2()
                        .text_sm()
                        .text_color(th.text)
                        .child("Theme"),
                )
                .child(choices)
                .when_some(settings_file, |el, path| {
                    el.child(
                        div()
                            .mt_10()
                            .pt_4()
                            .border_t_1()
                            .border_color(th.panel_border)
                            .text_xs()
                            .text_color(th.text_faint)
                            .child("Stored in"),
                    )
                    .child(
                        div()
                            .mt_1()
                            .font_family("Menlo")
                            .text_xs()
                            .text_color(th.text_faint)
                            // A long path gives way at the head: the file name
                            // is the part worth keeping on screen.
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            .child(path),
                    )
                }),
        )
        // The page is also the window's drag strip's neighbour; a press here
        // must not start moving the window.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}
