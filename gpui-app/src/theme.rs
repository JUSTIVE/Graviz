//! Dark/light palettes, resolved from the window's system appearance.

use gompass_core::graph::NodeKind;
use gpui::{rgb, rgba, Hsla, WindowAppearance};

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Hsla,
    pub panel: Hsla,
    pub panel_border: Hsla,
    pub card_bg: Hsla,
    pub card_border: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub text_faint: Hsla,
    pub hover_bg: Hsla,
    pub active_bg: Hsla,
    pub input_bg: Hsla,
    /// Translucent chrome (toolbar / status bar / tooltip) background.
    pub chrome_bg: Hsla,
    pub type_amber: Hsla,
    pub red: Hsla,
    pub overlay_green: Hsla,
    pub arg_orange: Hsla,
    pub accent: Hsla,
    pub shadow: Hsla,
    kind_object: Hsla,
    kind_interface: Hsla,
    kind_union: Hsla,
    kind_enum: Hsla,
    kind_input: Hsla,
    kind_scalar: Hsla,
}

impl Theme {
    pub fn kind_color(&self, kind: NodeKind) -> Hsla {
        match kind {
            NodeKind::Object => self.kind_object,
            NodeKind::Interface => self.kind_interface,
            NodeKind::Union => self.kind_union,
            NodeKind::Enum => self.kind_enum,
            NodeKind::Input => self.kind_input,
            NodeKind::Scalar => self.kind_scalar,
        }
    }
}

fn dark() -> Theme {
    Theme {
        bg: rgb(0x101216).into(),
        panel: rgb(0x14171d).into(),
        panel_border: rgb(0x242a35).into(),
        card_bg: rgb(0x1a1e26).into(),
        card_border: rgb(0x2c3340).into(),
        text: rgb(0xe6e9ef).into(),
        text_muted: rgb(0x8b93a3).into(),
        text_faint: rgb(0x687083).into(),
        hover_bg: rgb(0x232936).into(),
        active_bg: rgb(0x2a3140).into(),
        input_bg: rgb(0x1a1e26).into(),
        chrome_bg: rgba(0x14171df0).into(),
        type_amber: rgb(0xf59e0b).into(),
        red: rgb(0xe5534b).into(),
        overlay_green: rgb(0x34d399).into(),
        arg_orange: rgb(0xe08a4a).into(),
        accent: rgb(0x0ea5e9).into(),
        shadow: gpui::black().opacity(0.35),
        kind_object: rgb(0x0ea5e9).into(),
        kind_interface: rgb(0x8b5cf6).into(),
        kind_union: rgb(0xf59e0b).into(),
        kind_enum: rgb(0x10b981).into(),
        kind_input: rgb(0xd946ef).into(),
        kind_scalar: rgb(0xf43f5e).into(),
    }
}

fn light() -> Theme {
    Theme {
        bg: rgb(0xf2f4f7).into(),
        panel: rgb(0xe9ecf1).into(),
        panel_border: rgb(0xd4d9e1).into(),
        card_bg: rgb(0xffffff).into(),
        card_border: rgb(0xc9cfd9).into(),
        text: rgb(0x1c2129).into(),
        text_muted: rgb(0x5a6372).into(),
        text_faint: rgb(0x7c8595).into(),
        hover_bg: rgb(0xdde2ea).into(),
        active_bg: rgb(0xd2d9e4).into(),
        input_bg: rgb(0xffffff).into(),
        chrome_bg: rgba(0xf7f8faf0).into(),
        type_amber: rgb(0xb45309).into(),
        red: rgb(0xc93c34).into(),
        overlay_green: rgb(0x0d9668).into(),
        arg_orange: rgb(0xb05a1f).into(),
        accent: rgb(0x0369a1).into(),
        shadow: gpui::black().opacity(0.18),
        kind_object: rgb(0x0369a1).into(),
        kind_interface: rgb(0x5b21b6).into(),
        kind_union: rgb(0xb45309).into(),
        kind_enum: rgb(0x047857).into(),
        kind_input: rgb(0xa21caf).into(),
        kind_scalar: rgb(0xbe123c).into(),
    }
}

pub fn theme(appearance: WindowAppearance) -> Theme {
    // Debug overrides so automated selfshots can exercise both palettes.
    if std::env::var("GOMPASS_LIGHT").is_ok() {
        return light();
    }
    if std::env::var("GOMPASS_DARK").is_ok() {
        return dark();
    }
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => dark(),
        WindowAppearance::Light | WindowAppearance::VibrantLight => light(),
    }
}
