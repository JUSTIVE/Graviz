//! The settings flyout: theme, accent, scrolling. Saved as they change.

use nsh_core::prelude::*;
use nsh_metro::{Accent, Icon, TextStyle, radio_button};

use crate::{
    config::{self, ScrollMode, ThemeMode},
    store::Store,
    ui::{c, MONO},
};

struct Choice<V: 'static> {
    value: V,
    label: &'static str,
    blurb: &'static str,
}

const THEMES: &[Choice<ThemeMode>] = &[
    Choice { value: ThemeMode::Light, label: "light", blurb: "Always light." },
    Choice { value: ThemeMode::Dark, label: "dark", blurb: "Always dark." },
    Choice { value: ThemeMode::System, label: "system", blurb: "Follow macOS." },
];

const SCROLLING: &[Choice<ScrollMode>] = &[
    Choice { value: ScrollMode::Zoom, label: "zoom", blurb: "Scroll zooms at the cursor. ⇧ pans." },
    Choice { value: ScrollMode::Pan, label: "pan", blurb: "Scroll pans, sideways too. ⌘ zooms." },
];

fn radio_group<V: Copy + PartialEq + 'static>(
    current: impl Fn() -> V + Copy + 'static,
    options: &'static [Choice<V>],
    pick: impl Fn(V) + Copy + 'static,
) -> impl IntoElement {
    let mut col = view().gap(px(4.0));
    for choice in options {
        let value = choice.value;
        col = col.child(
            view()
                .flex_row()
                .items_center()
                .gap(px(16.0))
                .child(radio_button(move || current() == value).label(choice.label).on_select(move || pick(value)))
                .child(nsh_metro::text(TextStyle::Small, choice.blurb)),
        );
    }
    col
}

fn accent_grid(store: Store) -> impl IntoElement {
    const SWATCH: f32 = 64.;
    let current = move || crate::theme::accent_from_name(&store.settings.with(|s| s.accent.clone()));
    let mut grid = view().flex_row().flex_wrap().gap(px(10.));
    for a in Accent::ALL {
        let selected = move || current() == a;
        grid = grid.child(
            view()
                .w(px(SWATCH))
                .gap(px(4.0))
                .child(
                    view()
                        .size(px(SWATCH))
                        .items_end()
                        .justify_end()
                        .p(px(4.))
                        .bg(a.color())
                        .border(move || if selected() { px(3.) } else { px(0.) })
                        .border_color(move || nsh_metro::palette().foreground)
                        .hover(|s| s.opacity(0.85))
                        .cursor(Cursor::Pointer)
                        .on_press(move || {
                            nsh_metro::Theme::set_accent(a);
                            store.set_settings(|s| s.accent = a.name().into());
                        })
                        .child(when(selected, move || nsh_metro::icon(Icon::Check, 20., move || AdaptiveColor::from(a.ink())))),
                )
                .child(nsh_metro::text(TextStyle::Small, a.name()).color(move || {
                    let p = nsh_metro::palette();
                    AdaptiveColor::from(if selected() { p.foreground } else { p.subtle })
                })),
        );
    }
    grid
}

fn section_title(title: &'static str) -> impl IntoElement {
    view()
        .mt(px(40.))
        .mb(px(12.))
        .child(nsh_metro::text(TextStyle::AppTitle, title).color(move || AdaptiveColor::from(nsh_metro::palette().accent)))
}

fn setting_label(title: &'static str) -> impl IntoElement {
    view().mb(px(8.)).child(nsh_metro::text(TextStyle::Subtle, title))
}

pub fn page(store: Store) -> impl IntoElement {
    let settings_file = config::settings_path().map(|p| p.to_string_lossy().into_owned());
    view()
        .child(nsh_metro::text(TextStyle::Subtle, "Saved as you change them, and applied the next time you open a schema."))
        .child(section_title("Appearance"))
        .child(setting_label("Theme"))
        .child(radio_group(move || store.theme_mode(), THEMES, move |m| store.set_settings(|s| s.theme_mode = m)))
        .child(view().mt(px(32.)).child(setting_label("Accent colour")))
        .child(view().mb(px(12.)).child(
            nsh_metro::text(TextStyle::Large, move || {
                SharedString::from(crate::theme::accent_from_name(&store.settings.with(|s| s.accent.clone())).name())
            })
            .color(move || AdaptiveColor::from(nsh_metro::palette().accent)),
        ))
        .child(accent_grid(store))
        .child(section_title("Canvas"))
        .child(setting_label("Scrolling"))
        .child(radio_group(move || store.scroll_mode(), SCROLLING, move |m| store.set_settings(|s| s.scroll_mode = m)))
        .child(view().mt(px(8.)).child(nsh_metro::text(TextStyle::Small, "Dragging the canvas pans it either way.")))
        .child(when(settings_file.is_some(), move || {
            let path = settings_file.clone().unwrap_or_default();
            view()
                .child(view().mt(px(48.)).child(nsh_metro::text(TextStyle::Small, "Stored in")))
                .child(
                    text(path)
                        .mt(px(4.))
                        .font_family(MONO)
                        .text_size(px(12.))
                        .text_color(c(|t| t.text_faint))
                        .number_of_lines(1u32)
                        .text_overflow(nsh_core::EllipsizeMode::Head),
                )
        }))
}
