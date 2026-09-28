//! The Settings route: the choices that belong to the app rather than to the
//! schema in front of you.
//!
//! The view switches — descriptions, bundling, primitive fields, Relay — are
//! deliberately not here. Those belong to the graph you are reading, and they
//! live on its toolbar so you can watch what they do to it as you flip them.
//! What is left is what should still be true the next time you open a
//! different file.

use crate::config::ScrollMode;
use crate::icons::{icon, Icon};
use crate::theme::{Theme, ThemeMode};
use gpui::{div, prelude::*, px, MouseButton, SharedString, Window};

/// One card in a picker. The label doubles as the element id, so the labels
/// across a page have to stay distinct.
struct Choice<V: 'static> {
    value: V,
    icon: Icon,
    label: &'static str,
    blurb: &'static str,
}

const THEMES: &[Choice<ThemeMode>] = &[
    Choice { value: ThemeMode::Light, icon: Icon::Sun, label: "Light", blurb: "Always light." },
    Choice { value: ThemeMode::Dark, icon: Icon::Moon, label: "Dark", blurb: "Always dark." },
    Choice {
        value: ThemeMode::System,
        icon: Icon::Monitor,
        label: "System",
        blurb: "Follow macOS.",
    },
];

const SCROLLING: &[Choice<ScrollMode>] = &[
    Choice {
        value: ScrollMode::Zoom,
        icon: Icon::ZoomIn,
        label: "Zoom",
        blurb: "Scroll zooms at the cursor. ⇧ pans.",
    },
    Choice {
        value: ScrollMode::Pan,
        icon: Icon::Move,
        label: "Pan",
        blurb: "Scroll pans, sideways too. ⌘ zooms.",
    },
];

/// The options side by side rather than behind a button that cycles: a
/// settings page should show what the alternatives are.
fn picker<T: 'static, V: Copy + PartialEq + 'static>(
    th: Theme,
    current: V,
    options: &'static [Choice<V>],
    on_pick: impl Fn(&mut T, V, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let mut row = div().flex().gap_3();
    for choice in options {
        let active = choice.value == current;
        let pick = on_pick.clone();
        let value = choice.value;
        row = row.child(
            div()
                .id(choice.label)
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
                .on_click(cx.listener(move |this, _, window, cx| pick(this, value, window, cx)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(
                            choice.icon,
                            px(16.0),
                            if active { th.text } else { th.text_muted },
                        ))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(SharedString::from(choice.label)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(th.text_faint)
                        .child(SharedString::from(choice.blurb)),
                ),
        );
    }
    row
}

fn section_title(th: Theme, text: &'static str) -> impl IntoElement {
    div()
        .mt_8()
        .mb_3()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(th.text_faint)
        .child(SharedString::from(text.to_uppercase()))
}

fn setting_label(th: Theme, text: &'static str) -> impl IntoElement {
    div().mb_2().text_sm().text_color(th.text).child(SharedString::from(text))
}

pub struct SettingsProps {
    pub th: Theme,
    pub theme_mode: ThemeMode,
    pub scroll_mode: ScrollMode,
    /// Where the choices are written, named at the foot of the page.
    pub settings_file: Option<SharedString>,
}

pub fn view<T: 'static>(
    props: SettingsProps,
    on_theme: impl Fn(&mut T, ThemeMode, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    on_scroll: impl Fn(&mut T, ScrollMode, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let SettingsProps { th, theme_mode, scroll_mode, settings_file } = props;
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
                        .child(
                            "Saved as you change them, and applied the next time you open a schema.",
                        ),
                )
                .child(section_title(th, "Appearance"))
                .child(setting_label(th, "Theme"))
                .child(picker(th, theme_mode, THEMES, on_theme, cx))
                .child(section_title(th, "Canvas"))
                .child(setting_label(th, "Scrolling"))
                .child(picker(th, scroll_mode, SCROLLING, on_scroll, cx))
                .child(
                    div()
                        .mt_2()
                        .text_xs()
                        .text_color(th.text_faint)
                        .child("Dragging the canvas pans it either way."),
                )
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
        // The page sits under the title strip, which is the window's drag
        // handle; a press here must not start moving the window.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}
