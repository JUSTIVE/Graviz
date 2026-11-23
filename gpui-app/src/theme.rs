//! Dark/light palettes, resolved from the window's system appearance.

use graviz_core::graph::NodeKind;
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
    /// shadcn --primary: monochrome (near-black light / near-white dark).
    pub primary: Hsla,
    pub primary_fg: Hsla,
    /// Muted amber the web uses for builtin-scalar return types.
    pub type_builtin: Hsla,
    pub relay_orange: Hsla,
    /// Investigate outlines and pinned rows (orange-500).
    pub investigate: Hsla,
    pub pin: Hsla,
    /// Expired `[until]` marker (red-500) — same in both themes.
    pub expired: Hsla,
    #[allow(dead_code)]
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

// Metro grounds: pure black / pure white, one accent, no gradients. The
// chrome layer is WP's `PhoneChromeBrush` (#1F1F1F / #DDDDDD); borders are
// faint so the type, not boxes, carries the structure.
//
// Kind colours are the closest WP8 accents to the web's -500 series, so the
// graph keeps its semantics but reads as the same system as the chrome.
fn dark(accent: Hsla) -> Theme {
    Theme {
        bg: rgb(0x000000).into(),
        panel: rgb(0x0f0f0f).into(),
        panel_border: rgba(0xffffff26).into(),
        card_bg: rgb(0x1f1f1f).into(),
        card_border: rgba(0xffffff33).into(),
        text: rgb(0xffffff).into(),
        text_muted: rgba(0xffffff99).into(), // PhoneSubtleBrush
        text_faint: rgba(0xffffff66).into(), // PhoneDisabledBrush
        hover_bg: rgba(0xffffff1a).into(),
        active_bg: accent.opacity(0.22),
        input_bg: rgba(0xffffff1f).into(),
        chrome_bg: rgba(0x1f1f1ff2).into(),  // PhoneChromeBrush
        type_amber: rgb(0xf0a30a).into(),    // amber
        red: rgb(0xe51400).into(),           // red
        overlay_green: rgb(0x60a917).into(), // green
        arg_orange: rgb(0xfa6800).into(),    // orange
        accent,
        primary: accent,
        primary_fg: gpui::white(),
        type_builtin: rgb(0x87794e).into(), // taupe
        relay_orange: rgb(0xfa6800).into(),
        investigate: rgb(0xfa6800).into(),
        pin: rgb(0xfa6800).into(),
        expired: rgb(0xe51400).into(),
        shadow: gpui::black().opacity(0.0),
        kind_object: rgb(0x1ba1e2).into(),    // cyan
        kind_interface: rgb(0xaa00ff).into(), // violet
        kind_union: rgb(0xf0a30a).into(),     // amber
        kind_enum: rgb(0x60a917).into(),      // green
        kind_input: rgb(0xd80073).into(),     // magenta
        kind_scalar: rgb(0xe51400).into(),    // red
    }
}

fn light(accent: Hsla) -> Theme {
    Theme {
        bg: rgb(0xffffff).into(),
        panel: rgb(0xf7f7f7).into(),
        panel_border: rgba(0x00000021).into(),
        card_bg: rgb(0xf2f2f2).into(),
        card_border: rgba(0x00000033).into(),
        text: rgb(0x000000).into(),
        text_muted: rgba(0x00000099).into(),
        text_faint: rgba(0x00000066).into(),
        hover_bg: rgba(0x00000012).into(),
        active_bg: accent.opacity(0.16),
        input_bg: rgba(0x00000014).into(),
        chrome_bg: rgba(0xddddddf2).into(),
        type_amber: rgb(0xc98700).into(),
        red: rgb(0xe51400).into(),
        overlay_green: rgb(0x339933).into(),
        arg_orange: rgb(0xd35400).into(),
        accent,
        primary: accent,
        primary_fg: gpui::white(),
        type_builtin: rgb(0x87794e).into(),
        relay_orange: rgb(0xd35400).into(),
        investigate: rgb(0xfa6800).into(),
        pin: rgb(0xfa6800).into(),
        expired: rgb(0xe51400).into(),
        shadow: gpui::black().opacity(0.0),
        kind_object: rgb(0x1ba1e2).into(),
        kind_interface: rgb(0xaa00ff).into(),
        kind_union: rgb(0xf0a30a).into(),
        kind_enum: rgb(0x60a917).into(),
        kind_input: rgb(0xd80073).into(),
        kind_scalar: rgb(0xe51400).into(),
    }
}

/// The Metro accent the user picked (Settings → accent), read from the
/// `gpui_metro::Theme` global so the vendored components and the graph agree.
pub fn accent(cx: &gpui::App) -> gpui_metro::Accent {
    cx.try_global::<gpui_metro::Theme>()
        .map(|t| t.accent)
        .unwrap_or(gpui_metro::Accent::Cobalt)
}

pub fn accent_from_name(name: &str) -> gpui_metro::Accent {
    gpui_metro::Accent::ALL
        .into_iter()
        .find(|a| a.name() == name)
        .unwrap_or(gpui_metro::Accent::Cobalt)
}

pub fn set_accent(cx: &mut gpui::App, accent: gpui_metro::Accent) {
    gpui_metro::Theme::set_accent(accent, cx);
}

/// Keeps the metro components' dark/light ground in step with the mode this
/// app resolved (explicit choice, else the window's appearance). Call from a
/// root render; it only touches the global when the two disagree.
pub fn sync_metro(cx: &mut gpui::App, appearance: WindowAppearance) {
    let want = if is_dark(mode(cx), appearance) {
        gpui_metro::Mode::Dark
    } else {
        gpui_metro::Mode::Light
    };
    if cx
        .try_global::<gpui_metro::Theme>()
        .is_some_and(|t| t.mode != want)
    {
        gpui_metro::Theme::set_mode(want, cx);
    }
}

fn is_dark(mode: ThemeMode, appearance: WindowAppearance) -> bool {
    match mode {
        ThemeMode::Light => false,
        ThemeMode::Dark => true,
        ThemeMode::System => {
            if std::env::var("GRAVIZ_LIGHT").is_ok() {
                return false;
            }
            if std::env::var("GRAVIZ_DARK").is_ok() {
                return true;
            }
            matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            )
        }
    }
}

/// The web's three-way theme toggle (light → dark → system).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

/// App-wide theme choice, so every view resolves the same palette.
#[derive(Clone, Copy, Default)]
pub struct ThemeState(pub Option<ThemeMode>);

impl gpui::Global for ThemeState {}

pub fn mode(cx: &gpui::App) -> ThemeMode {
    cx.try_global::<ThemeState>()
        .and_then(|s| s.0)
        .unwrap_or(ThemeMode::System)
}

pub fn set_mode(cx: &mut gpui::App, mode: ThemeMode) {
    cx.set_global(ThemeState(Some(mode)));
}

/// The palette every view should use: the explicit choice, else the system,
/// tinted with the chosen Metro accent.
pub fn current(cx: &gpui::App, appearance: WindowAppearance) -> Theme {
    let accent = accent(cx).color();
    if is_dark(mode(cx), appearance) {
        dark(accent)
    } else {
        light(accent)
    }
}

/// Palette for an explicit mode (accent fixed to cobalt; for tests/previews).
#[allow(dead_code)]
pub fn theme_for(mode: ThemeMode, appearance: WindowAppearance) -> Theme {
    let accent = gpui_metro::Accent::Cobalt.color();
    if is_dark(mode, appearance) {
        dark(accent)
    } else {
        light(accent)
    }
}
