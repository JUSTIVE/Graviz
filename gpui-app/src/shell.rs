//! App shell: the title strip across the top (wordmark, open file, update
//! badge), the activity rail down the left, and the bottom-center commit
//! stamp.
//!
//! The web app put its routes in the header as text links and cycled the
//! theme from a button beside them. Here the routes are icons on a rail of
//! their own, outside every other pane, and the theme lives in Settings.

use crate::icons::{icon, Icon};
use crate::theme::Theme;
use gpui::{div, img, prelude::*, px, MouseButton, SharedString, Window};
use std::path::Path;

/// Left inset that keeps the header's contents clear of the window controls.
/// The titlebar is transparent and the traffic lights are drawn by the system
/// at (10, 10), so without this the wordmark sits on top of them.
#[cfg(target_os = "macos")]
const CONTROLS_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
const CONTROLS_INSET: f32 = 16.0;

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

/// Which view the rail highlights. `View` is the graph itself; the rest are
/// the full-window pages that replace it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    New,
    View,
    About,
    Settings,
}

/// A rail button's hover label. The rail is icons only, so the name has to
/// live somewhere: a real tooltip, rather than a caption that would double
/// the strip's width for something you read once.
struct Tip(SharedString);

impl gpui::Render for Tip {
    fn render(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(th.card_border)
            .bg(th.panel)
            .text_xs()
            .text_color(th.text)
            .child(self.0.clone())
    }
}

fn rail_button<T: 'static>(
    th: Theme,
    id: &'static str,
    ic: Icon,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    div()
        .id(id)
        .relative()
        .size(px(RAIL_W))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(!active, |el| el.hover(|el| el.bg(th.hover_bg)))
        .tooltip(move |_, cx| cx.new(|_| Tip(label.into())).into())
        .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
        .child(icon(ic, px(20.0), if active { th.text } else { th.text_faint }))
        // The selected item wears a bar on the rail's edge, the way VS Code
        // marks the active activity: it reads at a glance without asking the
        // icon to carry both "what" and "where you are".
        .when(active, |el| {
            el.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px((RAIL_W - 22.0) / 2.0))
                    .w(px(2.0))
                    .h(px(22.0))
                    .bg(th.primary),
            )
        })
}

/// Width of the activity rail, and of the square buttons on it.
const RAIL_W: f32 = 48.0;

/// The leftmost strip: the app's routes as icons, with Settings pinned at the
/// bottom. It sits outside everything else, so the schema sidebar and the
/// canvas both begin to its right.
pub fn rail<T: 'static>(
    th: Theme,
    route: Route,
    has_schema: bool,
    on_nav: impl Fn(&mut T, Route, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let (new, view, about, settings) =
        (on_nav.clone(), on_nav.clone(), on_nav.clone(), on_nav);
    div()
        .flex_none()
        .w(px(RAIL_W))
        .h_full()
        .flex()
        .flex_col()
        .justify_between()
        .bg(th.panel)
        .border_r_1()
        .border_color(th.panel_border)
        .child(
            div()
                .flex()
                .flex_col()
                .child(rail_button(
                    th,
                    "rail-new",
                    Icon::Upload,
                    "New schema",
                    route == Route::New,
                    move |this, window, cx| new(this, Route::New, window, cx),
                    cx,
                ))
                .when(has_schema, |el| {
                    el.child(rail_button(
                        th,
                        "rail-view",
                        Icon::Waypoints,
                        "Graph",
                        route == Route::View,
                        move |this, window, cx| view(this, Route::View, window, cx),
                        cx,
                    ))
                })
                .child(rail_button(
                    th,
                    "rail-about",
                    Icon::Info,
                    "About",
                    route == Route::About,
                    move |this, window, cx| about(this, Route::About, window, cx),
                    cx,
                )),
        )
        .child(rail_button(
            th,
            "rail-settings",
            Icon::Settings,
            "Settings (⌘,)",
            route == Route::Settings,
            move |this, window, cx| settings(this, Route::Settings, window, cx),
            cx,
        ))
}

/// The window's title strip. It carries the wordmark, the open file and any
/// update badge, and nothing clickable beyond that: the routes moved to the
/// rail and the theme moved into Settings.
pub fn header(
    th: Theme,
    // Open schema as `(file name, directory)`, from `file_label`.
    file: Option<(SharedString, Option<SharedString>)>,
    update_badge: Option<gpui::AnyElement>,
) -> impl IntoElement {
    div()
        .id("titlebar")
        .flex_none()
        .h(px(56.0))
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .pl(px(CONTROLS_INSET))
        .pr_4()
        .bg(th.bg)
        .border_b_1()
        .border_color(th.panel_border)
        // The header doubles as the titlebar (there is no system one): drag
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
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .text_color(th.text)
                .child(img(crate::icons::LOGO).size(px(20.0)).flex_none())
                .child(
                    div()
                        .text_base()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Graviz"),
                ),
        )
        .child(
            // Centred document label, the way a native titlebar names the file
            // that is open. It sits in the one column that can shrink, so a
            // long path gives way to the wordmark and the badge instead of
            // pushing them off the strip.
            div()
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .justify_center()
                .gap_2()
                .px_4()
                .when_some(file, |el, (name, dir)| {
                    el.child(
                        div()
                            .flex_none()
                            .max_w(px(280.0))
                            .truncate()
                            .text_sm()
                            .text_color(th.text)
                            .child(name),
                    )
                    .when_some(dir, |el, dir| {
                        el.child(
                            div()
                                .min_w(px(0.0))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                // A path is identified by its tail, so the head
                                // is what gives way.
                                .text_ellipsis_start()
                                .text_xs()
                                .text_color(th.text_muted)
                                .child(dir),
                        )
                    })
                }),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_3()
                .when_some(update_badge, |el, b| el.child(b)),
        )
}

/// Bottom-center commit stamp (10px mono, muted at 40%).
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
