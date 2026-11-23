//! The settings flyout's content: the choices that belong to the app rather
//! than to the schema in front of you. `gpui_metro::settings_flyout` draws
//! the panel, its accent header and its scrolling; this is what goes inside.
//!
//! The view switches — descriptions, bundling, primitive fields, Relay — are
//! deliberately not here. Those belong to the graph you are reading, and they
//! live on its toolbar so you can watch what they do to it as you flip them.
//! What is left is what should still be true the next time you open a
//! different file.

use crate::config::ScrollMode;
use crate::theme::{Theme, ThemeMode};
use gpui::{div, prelude::*, px, SharedString, Window};
use gpui_metro::{radio_button, text, Accent, Icon, TextStyle};

/// One radio in a group. The label doubles as the element id, so the labels
/// across a page have to stay distinct.
struct Choice<V: 'static> {
    value: V,
    label: &'static str,
    blurb: &'static str,
}

const THEMES: &[Choice<ThemeMode>] = &[
    Choice {
        value: ThemeMode::Light,
        label: "light",
        blurb: "Always light.",
    },
    Choice {
        value: ThemeMode::Dark,
        label: "dark",
        blurb: "Always dark.",
    },
    Choice {
        value: ThemeMode::System,
        label: "system",
        blurb: "Follow macOS.",
    },
];

const SCROLLING: &[Choice<ScrollMode>] = &[
    Choice {
        value: ScrollMode::Zoom,
        label: "zoom",
        blurb: "Scroll zooms at the cursor. ⇧ pans.",
    },
    Choice {
        value: ScrollMode::Pan,
        label: "pan",
        blurb: "Scroll pans, sideways too. ⌘ zooms.",
    },
];

/// Every option listed as a radio rather than hidden behind a control that
/// cycles: a settings page should show what the alternatives are.
fn radio_group<T: 'static, V: Copy + PartialEq + 'static>(
    current: V,
    options: &'static [Choice<V>],
    on_pick: impl Fn(&mut T, V, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let entity = cx.entity().downgrade();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(options.iter().map(|choice| {
            let pick = on_pick.clone();
            let entity = entity.clone();
            let value = choice.value;
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    radio_button(choice.label, choice.value == current)
                        .label(choice.label)
                        .on_select(move |w, cx| {
                            let pick = pick.clone();
                            entity.update(cx, |this, cx| pick(this, value, w, cx)).ok();
                        }),
                )
                .child(text(TextStyle::Small, choice.blurb))
        }))
}

/// WP8's accent picker: flat squares, the chosen one ringed and ticked.
fn accent_grid<T: 'static>(current: Accent, cx: &mut gpui::Context<T>) -> impl IntoElement {
    let p = gpui_metro::Theme::palette(cx);
    const SWATCH: f32 = 64.;
    div()
        .flex()
        .flex_wrap()
        .gap(px(10.))
        .children(Accent::ALL.into_iter().map(|a| {
            let selected = a == current;
            div()
                .flex()
                .flex_col()
                .gap_1()
                .w(px(SWATCH))
                .child(
                    div()
                        .id(SharedString::from(format!("accent-{}", a.name())))
                        .size(px(SWATCH))
                        .flex()
                        .items_end()
                        .justify_end()
                        .p(px(4.))
                        .bg(a.color())
                        .cursor_pointer()
                        .when(selected, |el| {
                            el.border(px(3.))
                                .border_color(p.foreground)
                                .child(Icon::Check.render(px(20.), a.ink()))
                        })
                        .when(!selected, |el| el.hover(|el| el.opacity(0.85)))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            crate::theme::set_accent(cx, a);
                            let mut s = crate::config::load_settings();
                            s.accent = a.name().into();
                            crate::config::save_settings(&s);
                            cx.notify();
                        })),
                )
                .child(text(TextStyle::Small, a.name()).color(if selected {
                    p.foreground
                } else {
                    p.subtle
                }))
        }))
}

/// Small-caps section header in the accent, as WP8 settings groups are.
fn section_title(color: gpui::Hsla, text_: &'static str) -> impl IntoElement {
    div()
        .mt_10()
        .mb_3()
        .child(text(TextStyle::AppTitle, text_).color(color))
}

fn setting_label(text_: &'static str) -> impl IntoElement {
    div().mb_2().child(text(TextStyle::Subtle, text_))
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
    let SettingsProps {
        th,
        theme_mode,
        scroll_mode,
        settings_file,
    } = props;
    let p = gpui_metro::Theme::palette(cx);
    let font = gpui_metro::Theme::font(cx);
    let accent = crate::theme::accent(cx);
    div()
        .flex()
        .flex_col()
        .font_family(font)
        .child(text(
            TextStyle::Subtle,
            "Saved as you change them, and applied the next time you open a schema.",
        ))
        .child(section_title(p.accent, "Appearance"))
        .child(setting_label("Theme"))
        .child(radio_group(theme_mode, THEMES, on_theme, cx))
        .child(div().mt_8().child(setting_label("Accent colour")))
        .child(
            div()
                .mb_3()
                .child(text(TextStyle::Large, accent.name()).color(p.accent)),
        )
        .child(accent_grid(accent, cx))
        .child(section_title(p.accent, "Canvas"))
        .child(setting_label("Scrolling"))
        .child(radio_group(scroll_mode, SCROLLING, on_scroll, cx))
        .child(div().mt_2().child(text(
            TextStyle::Small,
            "Dragging the canvas pans it either way.",
        )))
        .when_some(settings_file, |el, path| {
            el.child(div().mt_12().child(text(TextStyle::Small, "Stored in")))
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
        })
}
