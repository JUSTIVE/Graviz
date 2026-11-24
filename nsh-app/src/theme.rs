//! The palette. Metro grounds: pure black or pure white, one accent, no
//! gradients. Kind colours are the closest WP8 accents to the web app's -500
//! series, so the graph keeps its semantics and reads as the same system as
//! the chrome.

use graviz_core::graph::NodeKind;

/// Straight (not premultiplied) RGBA, already encoded for the screen.
///
/// The graph is drawn by the GPU and the chrome by nsh; this is the one
/// colour type both are fed from, so the two cannot drift apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const WHITE: Color = Color::hex(0xffffff);
    pub const BLACK: Color = Color::hex(0x000000);
    pub const CLEAR: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

    pub const fn hex(rgb: u32) -> Color {
        Color {
            r: ((rgb >> 16) & 0xff) as f32 / 255.0,
            g: ((rgb >> 8) & 0xff) as f32 / 255.0,
            b: (rgb & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub const fn hexa(rgba: u32) -> Color {
        let mut c = Color::hex(rgba >> 8);
        c.a = (rgba & 0xff) as f32 / 255.0;
        c
    }

    /// The same colour with its alpha scaled.
    pub fn opacity(self, factor: f32) -> Color {
        Color { a: self.a * factor.clamp(0.0, 1.0), ..self }
    }

    /// This colour composited over an opaque `base`, as an opaque colour.
    pub fn over(self, base: Color) -> Color {
        let mix = |b: f32, t: f32| b + (t - b) * self.a;
        Color { r: mix(base.r, self.r), g: mix(base.g, self.g), b: mix(base.b, self.b), a: 1.0 }
    }

    /// Mixed toward white by `amount`.
    pub fn lighten(self, amount: f32) -> Color {
        let up = |v: f32| v + (1.0 - v) * amount;
        Color { r: up(self.r), g: up(self.g), b: up(self.b), a: self.a }
    }

    pub fn array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    /// For nsh's views.
    pub fn hsla(self) -> nsh_core::prelude::Hsla {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        nsh_core::prelude::rgba((byte(self.r) << 24) | (byte(self.g) << 16) | (byte(self.b) << 8) | byte(self.a))
            .into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub bg: Color,
    pub panel: Color,
    pub panel_border: Color,
    pub card_bg: Color,
    pub card_border: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_faint: Color,
    pub hover_bg: Color,
    pub active_bg: Color,
    pub input_bg: Color,
    /// Translucent chrome (toolbar, status bar, tooltip) background.
    pub chrome_bg: Color,
    pub type_amber: Color,
    pub red: Color,
    pub overlay_green: Color,
    pub arg_orange: Color,
    pub accent: Color,
    /// Muted amber for builtin-scalar return types.
    pub type_builtin: Color,
    pub relay_orange: Color,
    /// Investigate outlines and pinned rows.
    pub investigate: Color,
    pub pin: Color,
    /// Expired `[until]` marker; the same in both themes.
    pub expired: Color,
    kind_object: Color,
    kind_interface: Color,
    kind_union: Color,
    kind_enum: Color,
    kind_input: Color,
    kind_scalar: Color,
}

impl Theme {
    pub fn kind_color(&self, kind: NodeKind) -> Color {
        match kind {
            NodeKind::Object => self.kind_object,
            NodeKind::Interface => self.kind_interface,
            NodeKind::Union => self.kind_union,
            NodeKind::Enum => self.kind_enum,
            NodeKind::Input => self.kind_input,
            NodeKind::Scalar => self.kind_scalar,
        }
    }

    pub fn dark(accent: Color) -> Theme {
        Theme {
            dark: true,
            bg: Color::hex(0x000000),
            panel: Color::hex(0x0f0f0f),
            panel_border: Color::hexa(0xffffff26),
            card_bg: Color::hex(0x1f1f1f),
            card_border: Color::hexa(0xffffff33),
            text: Color::hex(0xffffff),
            text_muted: Color::hexa(0xffffff99),
            text_faint: Color::hexa(0xffffff66),
            hover_bg: Color::hexa(0xffffff1a),
            active_bg: accent.opacity(0.22),
            input_bg: Color::hexa(0xffffff1f),
            chrome_bg: Color::hexa(0x1f1f1ff2),
            type_amber: Color::hex(0xf0a30a),
            red: Color::hex(0xe51400),
            overlay_green: Color::hex(0x60a917),
            arg_orange: Color::hex(0xfa6800),
            accent,
            type_builtin: Color::hex(0x87794e),
            relay_orange: Color::hex(0xfa6800),
            investigate: Color::hex(0xfa6800),
            pin: Color::hex(0xfa6800),
            expired: Color::hex(0xe51400),
            kind_object: Color::hex(0x1ba1e2),
            kind_interface: Color::hex(0xaa00ff),
            kind_union: Color::hex(0xf0a30a),
            kind_enum: Color::hex(0x60a917),
            kind_input: Color::hex(0xd80073),
            kind_scalar: Color::hex(0xe51400),
        }
    }

    pub fn light(accent: Color) -> Theme {
        Theme {
            dark: false,
            bg: Color::hex(0xffffff),
            panel: Color::hex(0xf7f7f7),
            panel_border: Color::hexa(0x00000021),
            card_bg: Color::hex(0xf2f2f2),
            card_border: Color::hexa(0x00000033),
            text: Color::hex(0x000000),
            text_muted: Color::hexa(0x00000099),
            text_faint: Color::hexa(0x00000066),
            hover_bg: Color::hexa(0x00000012),
            active_bg: accent.opacity(0.16),
            input_bg: Color::hexa(0x00000014),
            chrome_bg: Color::hexa(0xddddddf2),
            type_amber: Color::hex(0xc98700),
            overlay_green: Color::hex(0x339933),
            arg_orange: Color::hex(0xd35400),
            relay_orange: Color::hex(0xd35400),
            ..Theme::dark(accent)
        }
        .with_dark(false)
    }

    fn with_dark(mut self, dark: bool) -> Theme {
        self.dark = dark;
        self
    }
}

/// The theme now: the Metro ground and accent (tracked, so a closure that
/// reads it follows a change of either).
pub fn current() -> Theme {
    let t = nsh_metro::Theme::get();
    let accent = color_of(t.accent.color());
    match t.mode {
        nsh_metro::Mode::Dark => Theme::dark(accent),
        nsh_metro::Mode::Light => Theme::light(accent),
    }
}

/// An nsh colour as ours.
pub fn color_of(c: nsh_core::prelude::Hsla) -> Color {
    let r = nsh_core::prelude::Rgba::from(c);
    Color { r: r.r, g: r.g, b: r.b, a: r.a }
}

/// The accent picked in Settings, by name, else cobalt.
pub fn accent_from_name(name: &str) -> nsh_metro::Accent {
    nsh_metro::Accent::ALL.into_iter().find(|a| a.name() == name).unwrap_or(nsh_metro::Accent::Cobalt)
}

/// `0xRRGGBB` of an accent, for the graph view's prop.
pub fn accent_hex(a: nsh_metro::Accent) -> u32 {
    let c = color_of(a.color());
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (b(c.r) << 16) | (b(c.g) << 8) | b(c.b)
}
