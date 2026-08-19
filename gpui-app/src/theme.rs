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

// Base palette mirrors the web app's shadcn OKLCH variables converted to
// sRGB (Tailwind `neutral`): --background/--card/--muted/--border/…
fn dark() -> Theme {
    Theme {
        bg: rgb(0x0a0a0a).into(),          // --background
        panel: rgb(0x171717).into(),       // --sidebar / --card
        panel_border: rgba(0xffffff1a).into(), // --border: white/10%
        card_bg: rgb(0x171717).into(),     // --card
        card_border: rgba(0xffffff1a).into(),
        text: rgb(0xfafafa).into(),        // --foreground
        text_muted: rgb(0xa1a1a1).into(),  // --muted-foreground
        text_faint: rgb(0x737373).into(),
        hover_bg: rgb(0x262626).into(),    // --accent
        active_bg: rgb(0x262626).into(),   // --secondary
        input_bg: rgba(0xffffff26).into(), // --input: white/15%
        chrome_bg: rgba(0x171717f0).into(), // --popover, translucent
        type_amber: rgb(0xf59e0b).into(),
        red: rgb(0xff6467).into(),         // --destructive
        overlay_green: rgb(0x10b981).into(),
        arg_orange: rgb(0xe08a4a).into(),
        accent: rgb(0x0ea5e9).into(),
        primary: rgb(0xe5e5e5).into(),
        primary_fg: rgb(0x171717).into(),
        type_builtin: rgb(0xb08c5a).into(),
        relay_orange: rgb(0xf26a03).into(),
        investigate: rgb(0xf97316).into(),
        pin: rgb(0xf97316).into(),
        expired: rgb(0xef4444).into(),
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
        bg: rgb(0xfafaf9).into(),          // --background
        panel: rgb(0xfafafa).into(),       // --sidebar
        panel_border: rgb(0xe5e5e5).into(), // --border
        card_bg: rgb(0xffffff).into(),     // --card
        card_border: rgb(0xe5e5e5).into(),
        text: rgb(0x0a0a0a).into(),        // --foreground
        text_muted: rgb(0x737373).into(),  // --muted-foreground
        text_faint: rgb(0xa1a1a1).into(),
        hover_bg: rgb(0xf5f5f5).into(),    // --accent
        active_bg: rgb(0xf5f5f5).into(),   // --secondary
        input_bg: rgb(0xffffff).into(),
        chrome_bg: rgba(0xfffffff0).into(), // --popover, translucent
        type_amber: rgb(0xf59e0b).into(),
        red: rgb(0xe7000b).into(),         // --destructive
        overlay_green: rgb(0x10b981).into(),
        arg_orange: rgb(0xb05a1f).into(),
        accent: rgb(0x0369a1).into(),
        primary: rgb(0x171717).into(),
        primary_fg: rgb(0xfafafa).into(),
        type_builtin: rgb(0xb08c5a).into(),
        relay_orange: rgb(0xf26a03).into(),
        investigate: rgb(0xf97316).into(),
        pin: rgb(0xf97316).into(),
        expired: rgb(0xef4444).into(),
        shadow: gpui::black().opacity(0.18),
        // KIND_COLORS is theme-independent in the web app (KIND_COLORS_DARK
        // exists but has no consumer), so light uses the same -500 series.
        kind_object: rgb(0x0ea5e9).into(),
        kind_interface: rgb(0x8b5cf6).into(),
        kind_union: rgb(0xf59e0b).into(),
        kind_enum: rgb(0x10b981).into(),
        kind_input: rgb(0xd946ef).into(),
        kind_scalar: rgb(0xf43f5e).into(),
    }
}

/// The web's three-way theme toggle (light → dark → system).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub fn next(self) -> Self {
        match self {
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
            ThemeMode::System => ThemeMode::Light,
        }
    }
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

/// The palette every view should use: the explicit choice, else the system.
pub fn current(cx: &gpui::App, appearance: WindowAppearance) -> Theme {
    theme_for(mode(cx), appearance)
}

/// Palette for an explicit mode, falling back to the window appearance.
pub fn theme_for(mode: ThemeMode, appearance: WindowAppearance) -> Theme {
    match mode {
        ThemeMode::Light => light(),
        ThemeMode::Dark => dark(),
        ThemeMode::System => theme(appearance),
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
