//! Lucide icons, embedded into the binary at compile time.
//!
//! The SVGs under `assets/icons/` are the same shapes the web app renders
//! (generated from the `lucide-react` package the web app depends on). GPUI
//! rasterizes an SVG into an alpha mask and tints it with the element's text
//! color, so the files carry `stroke="currentColor"` and no embedded styles.

use gpui::{prelude::*, svg, AssetSource, Hsla, Pixels, Result, SharedString};
use std::borrow::Cow;

macro_rules! define_icons {
    ($($variant:ident => $file:literal,)*) => {
        /// A lucide icon bundled with the app.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $(
                #[doc = concat!("The lucide `", $file, "` icon.")]
                $variant,
            )*
        }

        impl Icon {
            /// Every bundled icon, in declaration order.
            #[allow(dead_code)]
            pub const ALL: &'static [Icon] = &[$(Icon::$variant,)*];

            /// The asset path this icon is registered under, e.g. `"icons/search.svg"`.
            pub const fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => concat!("icons/", $file, ".svg"),)*
                }
            }
        }

        /// `(asset path, file contents)` for every bundled icon.
        const EMBEDDED: &[(&str, &[u8])] = &[
            $((
                concat!("icons/", $file, ".svg"),
                include_bytes!(concat!("../assets/icons/", $file, ".svg")),
            ),)*
        ];
    };
}

define_icons! {
    Search => "search",
    X => "x",
    Filter => "filter",
    History => "history",
    Trash2 => "trash-2",
    ChevronUp => "chevron-up",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    ArrowRight => "arrow-right",
    Microscope => "microscope",
    PanelLeftOpen => "panel-left-open",
    PanelLeftClose => "panel-left-close",
    Waypoints => "waypoints",
    Unlink => "unlink",
    Clock => "clock",
    Layers => "layers",
    Highlighter => "highlighter",
    Sun => "sun",
    Moon => "moon",
    Monitor => "monitor",
    Upload => "upload",
    Link2 => "link-2",
    Sparkles => "sparkles",
    Wand2 => "wand-2",
    TriangleAlert => "triangle-alert",
    CircleAlert => "circle-alert",
    LoaderCircle => "loader-circle",
}

/// Asset path of the app wordmark's logo — the compass the web app uses as
/// its favicon. It is a full-colour raster, not a tintable mask, so it is an
/// image rather than one of the SVG icons above.
pub const LOGO: &str = "logo.png";

/// Render `name` as a square `size`×`size` glyph tinted with `color`.
pub fn icon(name: Icon, size: Pixels, color: Hsla) -> impl IntoElement {
    svg()
        .path(name.path())
        .size(size)
        .text_color(color)
        .flex_none()
}

/// Serves the embedded icons to GPUI. Register with
/// `application().with_assets(icons::Assets)`.
pub struct Assets;

const LOGO_BYTES: &[u8] = include_bytes!("../assets/logo.png");

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(LOGO_BYTES)));
        }
        Ok(EMBEDDED
            .iter()
            .find(|(asset_path, _)| *asset_path == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(EMBEDDED
            .iter()
            .filter(|(asset_path, _)| asset_path.starts_with(path))
            .map(|(asset_path, _)| SharedString::from(*asset_path))
            .collect())
    }
}

#[allow(dead_code)]
fn _typecheck() {
    let _ = icon(Icon::Search, gpui::px(12.0), gpui::white());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_embedded_and_loadable() {
        for &name in Icon::ALL {
            let bytes = Assets
                .load(name.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{} is not embedded", name.path()));
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.starts_with("<svg"), "{}", name.path());
            assert!(text.contains("currentColor"), "{}", name.path());
            assert!(text.contains(r#"viewBox="0 0 24 24""#), "{}", name.path());
        }
    }

    /// GPUI renders an icon by rasterizing it and keeping only the alpha
    /// channel, so a file whose paint fails to resolve renders as nothing.
    #[test]
    fn every_icon_rasterizes_to_visible_pixels() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(Assets));
        for &name in Icon::ALL {
            let bytes = Assets.load(name.path()).unwrap().unwrap();
            let image = renderer
                .render_single_frame(&bytes, 1.0)
                .unwrap_or_else(|e| panic!("{} failed to rasterize: {e}", name.path()));
            let opaque = image
                .as_bytes(0)
                .unwrap()
                .chunks_exact(4)
                .filter(|px| px[3] > 0)
                .count();
            assert!(opaque > 0, "{} rasterized to nothing", name.path());
        }
    }

    #[test]
    fn unknown_paths_load_as_none() {
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
        assert_eq!(Assets.list("icons/").unwrap().len(), Icon::ALL.len());
    }
}
