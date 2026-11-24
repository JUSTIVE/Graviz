//! The app's own styled pieces, shared by every screen: colours from the
//! theme, the kind badge, the Lucide glyphs Metro's icon set lacks.

use graviz_core::graph::NodeKind;
use nsh_core::prelude::*;

use crate::theme::{self, Color, Theme};

/// The theme now (tracked).
pub fn th() -> Theme {
    theme::current()
}

/// A theme colour, as a binding: `.bg(c(|t| t.panel))`.
pub fn c(f: fn(&Theme) -> Color) -> impl Fn() -> Hsla + Copy + 'static {
    move || f(&th()).hsla()
}

pub const MONO: &str = "Menlo";

/// Lowercase kind words, as the web's badges show them.
pub fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Object => "type",
        NodeKind::Interface => "interface",
        NodeKind::Union => "union",
        NodeKind::Enum => "enum",
        NodeKind::Input => "input",
        NodeKind::Scalar => "scalar",
    }
}

/// The solid kind badge used in list rows (web `KIND_STYLES[kind].badge`).
pub fn kind_badge(kind: NodeKind, label: impl Into<SharedString>) -> View {
    view()
        .flex_none()
        .px(px(6.0))
        .bg(move || th().kind_color(kind).hsla())
        .child(text(label.into()).text_size(px(9.0)).line_height(px(14.0)).text_color(rgb(0xffffff)))
}

/// A Lucide glyph from `assets/icons`, tinted with `color`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Glyph {
    ChevronUp,
    ChevronDown,
    PanelLeftOpen,
    PanelLeftClose,
    Layers,
    Link2,
}

impl Glyph {
    fn markup(self) -> &'static str {
        match self {
            Glyph::ChevronUp => include_str!("../assets/icons/chevron-up.svg"),
            Glyph::ChevronDown => include_str!("../assets/icons/chevron-down.svg"),
            Glyph::PanelLeftOpen => include_str!("../assets/icons/panel-left-open.svg"),
            Glyph::PanelLeftClose => include_str!("../assets/icons/panel-left-close.svg"),
            Glyph::Layers => include_str!("../assets/icons/layers.svg"),
            Glyph::Link2 => include_str!("../assets/icons/link-2.svg"),
        }
    }
}

pub fn glyph(g: Glyph, size: f32, color: impl Fn() -> Hsla + 'static) -> impl IntoElement {
    svg(nsh_core::element::ImageSrc::svg_markup(g.markup())).size(px(size)).text_color(color)
}

/// A Metro icon in the theme's colour.
pub fn icon(i: nsh_metro::Icon, size: f32, color: impl Fn() -> Hsla + 'static) -> impl IntoElement {
    nsh_metro::icon(i, size, move || AdaptiveColor::from(color()))
}

/// Text in the monospace face.
pub fn mono(s: impl IntoReactive<SharedString>, size: f32) -> Text {
    text(s).font_family(MONO).text_size(px(size))
}

/// Text in the Metro face.
pub fn metro(s: impl IntoReactive<SharedString>, size: f32) -> Text {
    nsh_metro::label(s).text_size(px(size))
}

/// `~/dir` for a path under home.
pub fn tilde(dir: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && dir == h => "~".to_string(),
        Ok(h) if !h.is_empty() && dir.starts_with(&format!("{h}/")) => format!("~{}", &dir[h.len()..]),
        _ => dir.to_string(),
    }
}

/// Splits a schema path into the file's name and a shortened directory. A
/// pasted schema has no directory.
pub fn file_label(path: &std::path::Path) -> (String, Option<String>) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let dir = path.parent().map(|p| p.to_string_lossy().into_owned()).filter(|d| !d.is_empty()).map(|d| tilde(&d));
    (name, dir)
}

/// The composite of a translucent colour over the ground, for places nsh
/// would otherwise blend twice.
pub fn over_bg(c: Color) -> Color {
    c.over(th().bg)
}

/// gpui-metro's hyperlink at 0.7 scale, as the gpui app drew its small
/// links: Metro's medium size underlined in the foreground.
pub fn small_link(label: impl Into<String>, f: impl Fn() + 'static) -> impl IntoElement {
    view()
        .cursor(Cursor::Pointer)
        .active(|s| s.opacity(0.6))
        .on_press(f)
        .child(
            nsh_metro::label(label.into())
                .text_size(px(nsh_metro::theme::size::MEDIUM * 0.7))
                .underline()
                .text_color(move || nsh_metro::palette().foreground),
        )
}

/// nsh-metro's check box at the 0.6 scale the canvas's floating controls
/// use: its 32pt outlined box, 22pt check and MEDIUM label, all × 0.6.
pub fn small_check(
    checked: impl Fn() -> bool + Copy + 'static,
    label: &'static str,
    on_change: impl Fn(bool) + 'static,
) -> impl IntoElement {
    const K: f32 = 0.6;
    let fg = move || nsh_metro::palette().foreground;
    view()
        .flex_none()
        .flex_row()
        .items_center()
        .gap(px(14.0 * K))
        .py(px(6.0 * K))
        .cursor(Cursor::Pointer)
        .active(|s| s.opacity(0.7))
        .on_press(move || on_change(!checked()))
        .child(
            view()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(px(32.0 * K))
                .border(px(2.0 * K))
                .border_color(fg)
                .child(view().opacity(move || if checked() { 1f32 } else { 0f32 }).child(nsh_metro::Icon::Check.render(22.0 * K, fg))),
        )
        .child(
            nsh_metro::label(label)
                .flex_none()
                .font_family(nsh_metro::theme::FONT_FAMILY)
                .text_size(px(nsh_metro::theme::size::MEDIUM * K))
                .text_color(fg),
        )
}

/// gpui-metro's round button over its lowercase label, at app-bar size.
pub fn bar_button(
    glyph: nsh_metro::Icon,
    label: impl Fn() -> String + 'static,
    enabled: impl Fn() -> bool + Copy + 'static,
    filled: impl Fn() -> bool + Copy + 'static,
    on_click: impl Fn() + 'static,
) -> impl IntoElement {
    // The round button takes its own press, so both it and the label run
    // the action.
    let on_click = std::rc::Rc::new(on_click);
    let on_label = on_click.clone();
    view()
        .flex_none()
        .items_center()
        .gap(px(2.0))
        .w(px(60.0))
        .cursor(Cursor::Pointer)
        .on_press(move || {
            if enabled() {
                on_label();
            }
        })
        .child(
            nsh_metro::round_button(glyph)
                .diameter(30.0)
                .disabled(move || !enabled())
                .filled(filled)
                .on_click(move || on_click()),
        )
        .child(
            nsh_metro::label(label)
                .text_size(px(10.0))
                .line_height(px(12.0))
                .text_color(move || if enabled() { th().text.hsla() } else { th().text_faint.hsla() })
                .truncate(),
        )
}

/// Binds up and down even while a text field has the focus: in the search
/// boxes they walk the results, as in the gpui app. `step` gets -1 or 1.
pub fn arrow_keys(step: impl Fn(i32) + Copy + 'static) {
    let always = nsh_core::prelude::KeyOptions { when: nsh_core::keymap::When::Always, repeat: true, ..Default::default() };
    nsh_core::prelude::on_key_with("up", always, move |_| step(-1));
    nsh_core::prelude::on_key_with("down", always, move |_| step(1));
}
