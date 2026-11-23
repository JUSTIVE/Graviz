//! App shell, in Metro: the title strip across the top (app title, open
//! file, update link), a pivot-style row of route words under it, and a
//! subtle commit stamp in the corner.

use crate::theme::Theme;
use gpui::{div, prelude::*, px, MouseButton, SharedString, Window};
use std::path::Path;

/// Left inset that keeps the strip's contents clear of the window controls.
/// The titlebar is transparent and the traffic lights are drawn by the system
/// just inside it, so without this the title sits on top of them.
#[cfg(target_os = "macos")]
const CONTROLS_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
const CONTROLS_INSET: f32 = 16.0;

/// Height of the title strip: the standard macOS titlebar, not the tall
/// header the web app needed to hold a nav. The traffic lights in `main.rs`
/// are centred against this, so the two move together.
pub const TITLEBAR_H: f32 = 28.0;

/// Short commit the build was made from, stamped bottom-center like the web.
pub const COMMIT: Option<&str> = option_env!("GRAVIZ_COMMIT");

/// Splits a schema path into what the titlebar shows: the file name, and the
/// directory holding it with `$HOME` collapsed to `~`.
///
/// `home` is passed in rather than read here so the split is testable without
/// a real environment. A pasted schema has no directory, and neither does a
/// bare relative name.
pub fn file_label(path: &Path, home: Option<&str>) -> (SharedString, Option<SharedString>) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let dir = path
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|d| !d.is_empty())
        .map(|d| match home {
            Some(h) if !h.is_empty() && d == h => "~".to_string(),
            Some(h) if !h.is_empty() && d.starts_with(&format!("{h}/")) => {
                format!("~{}", &d[h.len()..])
            }
            _ => d,
        });
    (name.into(), dir.map(SharedString::from))
}

/// Which view the nav marks as current. `View` is the graph itself; the rest
/// are the full-window pages that replace it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    New,
    View,
    About,
    Settings,
}

/// Size of the nav words: a Metro pivot header scaled for a desktop window
/// (the phone's were 48), big enough that type alone carries the structure.
const NAV_SIZE: f32 = 30.0;

/// Left margin the nav and the title strip's text share. On macOS the title
/// strip has to clear the traffic lights, so only the nav row uses it.
const NAV_MARGIN: f32 = 18.0;

/// The routes as a Metro pivot header row: lower-case light words, the
/// current one in the foreground and the rest subtle. Unlike a phone pivot
/// the order stays fixed (a desktop nav is read as a menu, not swiped), so
/// this draws the same look itself rather than using `gpui_metro::pivot`,
/// which rotates the selected header to the front. "graph" only exists once
/// a schema is open.
pub fn nav<T: 'static>(
    route: Route,
    has_schema: bool,
    on_nav: impl Fn(&mut T, Route, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let p = gpui_metro::Theme::palette(cx);
    let items: &[(Route, &'static str, &'static str)] = &[
        (Route::View, "nav-graph", "graph"),
        (Route::New, "nav-new", "new"),
        (Route::Settings, "nav-settings", "settings"),
        (Route::About, "nav-about", "about"),
    ];
    let words = items
        .iter()
        .filter(|(r, ..)| has_schema || *r != Route::View)
        .map(|&(r, id, label)| {
            let active = r == route;
            let on_nav = on_nav.clone();
            div()
                .id(id)
                .flex_none()
                .text_size(px(NAV_SIZE))
                .line_height(px(NAV_SIZE * 1.2))
                .font_weight(gpui_metro::theme::LIGHT)
                .text_color(if active { p.foreground } else { p.subtle })
                .cursor_pointer()
                .when(!active, |el| {
                    el.hover(move |el| el.text_color(p.foreground))
                })
                .when(r == Route::Settings, |el| {
                    el.tooltip(|_, cx| cx.new(|_| Tip("Settings (⌘,)".into())).into())
                })
                .on_click(cx.listener(move |this, _, window, cx| on_nav(this, r, window, cx)))
                .child(label)
        });
    div()
        .id("nav")
        .flex_none()
        .w_full()
        .flex()
        .items_end()
        .gap(px(22.0))
        .pl(px(NAV_MARGIN))
        .pr(px(NAV_MARGIN))
        .pt(px(2.0))
        .pb(px(8.0))
        .overflow_hidden()
        .font_family(gpui_metro::Theme::font(cx))
        .bg(p.background)
        .children(words)
}

/// A hover label for the one nav word with a shortcut worth telling.
struct Tip(SharedString);

impl gpui::Render for Tip {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let p = gpui_metro::Theme::palette(cx);
        div()
            .px_2()
            .py_1()
            .border_1()
            .border_color(p.foreground)
            .bg(p.background)
            .font_family(gpui_metro::Theme::font(cx))
            .text_size(px(gpui_metro::theme::size::SMALL))
            .text_color(p.foreground)
            .child(self.0.clone())
    }
}

/// The window's title strip: the Metro app title "GRAVIZ", then the open
/// file and its directory in subtle text, and any update link at the right.
pub fn header(
    th: Theme,
    // Open schema as `(file name, directory)`, from `file_label`.
    file: Option<(SharedString, Option<SharedString>)>,
    update_badge: Option<gpui::AnyElement>,
    cx: &gpui::App,
) -> impl IntoElement {
    let p = gpui_metro::Theme::palette(cx);
    div()
        .id("titlebar")
        .flex_none()
        .h(px(TITLEBAR_H))
        .w_full()
        .flex()
        .items_center()
        .gap(px(12.0))
        .pl(px(CONTROLS_INSET))
        .pr(px(NAV_MARGIN))
        .bg(th.bg)
        .font_family(gpui_metro::Theme::font(cx))
        // The strip doubles as the titlebar (there is no system one): drag
        // moves the window, and a double-click does whatever the system's
        // "double-click a window's title bar to" setting says — usually zoom,
        // which is the maximize toggle.
        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
        .on_click(|ev, window, _| {
            if ev.click_count() == 2 {
                #[cfg(target_os = "macos")]
                window.titlebar_double_click();
                #[cfg(not(target_os = "macos"))]
                window.zoom_window();
            }
        })
        .child(
            div()
                .flex_none()
                .text_size(px(12.0))
                .font_weight(gpui_metro::theme::SEMIBOLD)
                .text_color(p.foreground)
                .child("GRAVIZ"),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .overflow_hidden()
                .text_size(px(12.0))
                .text_color(p.subtle)
                .when_some(file, |el, (name, dir)| {
                    el.child(div().flex_none().max_w(px(280.0)).truncate().child(name))
                        .when_some(dir, |el, dir| {
                            el.child(
                                div()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    // A path is identified by its tail, so the
                                    // head is what gives way.
                                    .text_ellipsis_start()
                                    .text_color(p.subtle.opacity(0.7))
                                    .child(dir),
                            )
                        })
                }),
        )
        .when_some(update_badge, |el, b| el.child(div().flex_none().child(b)))
}

/// Bottom-center commit stamp (10px mono, muted at 40%): there to read when
/// you look for it.
pub fn commit_badge(th: Theme) -> Option<impl IntoElement> {
    COMMIT.map(|c| {
        div()
            .absolute()
            .bottom(px(8.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .text_size(px(10.0))
                    .font_family("Menlo")
                    .text_color(th.text_muted.opacity(0.4))
                    .child(SharedString::from(c)),
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_label_splits_the_name_from_its_directory() {
        let (name, dir) = file_label(Path::new("/Users/b/git/crepe/schema.graphql"), Some("/Users/b"));
        assert_eq!(&*name, "schema.graphql");
        assert_eq!(dir.as_deref(), Some("~/git/crepe"));
    }

    #[test]
    fn file_label_leaves_a_pasted_schema_without_a_directory() {
        let (name, dir) = file_label(Path::new("(pasted)"), Some("/Users/b"));
        assert_eq!(&*name, "(pasted)");
        assert_eq!(dir, None, "a pasted schema is not a file on disk");
    }

    #[test]
    fn file_label_collapses_home_but_not_a_lookalike_sibling() {
        let (_, dir) = file_label(Path::new("/Users/b/a.graphql"), Some("/Users/b"));
        assert_eq!(dir.as_deref(), Some("~"), "home itself");
        let (_, dir) = file_label(Path::new("/Users/bison/a.graphql"), Some("/Users/b"));
        assert_eq!(dir.as_deref(), Some("/Users/bison"), "shares a prefix, not a parent");
    }
}
