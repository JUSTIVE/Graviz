//! App shell: the sticky header the web app puts above every route
//! (wordmark, nav, theme toggle) plus the bottom-center commit stamp.

use crate::icons::{icon, Icon};
use crate::theme::{Theme, ThemeMode};
use gpui::{div, img, prelude::*, px, MouseButton, SharedString, Stateful, Window};

/// Left inset that keeps the header's contents clear of the window controls.
/// The titlebar is transparent and the traffic lights are drawn by the system
/// at (10, 10), so without this the wordmark sits on top of them.
#[cfg(target_os = "macos")]
const CONTROLS_INSET: f32 = 78.0;
#[cfg(not(target_os = "macos"))]
const CONTROLS_INSET: f32 = 16.0;

/// Short commit the build was made from, stamped bottom-center like the web.
pub const COMMIT: Option<&str> = option_env!("GRAVIZ_COMMIT");

/// Which route the shell highlights in its nav.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    New,
    View,
    About,
}

fn nav_link(th: Theme, id: &'static str, label: &'static str, active: bool) -> Stateful<gpui::Div> {
    div()
        .id(id)
        // The header is also the window's drag strip; without this a press on
        // a nav link would start moving the window instead of navigating.
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .rounded_md()
        .px_3()
        .py(px(6.0))
        .text_sm()
        .cursor_pointer()
        .when(active, |el| el.bg(th.active_bg).text_color(th.text))
        .when(!active, |el| {
            el.text_color(th.text_muted).hover(|el| el.bg(th.hover_bg).text_color(th.text))
        })
        .child(SharedString::from(label))
}

/// The sticky app header. `on_nav` fires with the clicked route, `on_theme`
/// cycles light → dark → system like the web's single-button toggle.
#[allow(clippy::too_many_arguments)]
pub fn header<T: 'static>(
    th: Theme,
    route: Route,
    has_schema: bool,
    theme_mode: ThemeMode,
    update_badge: Option<gpui::AnyElement>,
    on_nav: impl Fn(&mut T, Route, &mut Window, &mut gpui::Context<T>) + 'static + Clone,
    on_theme: impl Fn(&mut T, &mut Window, &mut gpui::Context<T>) + 'static,
    cx: &mut gpui::Context<T>,
) -> impl IntoElement {
    let (theme_icon, theme_label) = match theme_mode {
        ThemeMode::Light => (Icon::Sun, "Light"),
        ThemeMode::Dark => (Icon::Moon, "Dark"),
        ThemeMode::System => (Icon::Monitor, "System"),
    };
    let on_nav_new = on_nav.clone();
    let on_nav_view = on_nav.clone();
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
                .items_center()
                .gap_6()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_color(th.text)
                        .child(img(crate::icons::LOGO).size(px(20.0)).flex_none())
                        .child(div().text_base().font_weight(gpui::FontWeight::SEMIBOLD).child("Graviz")),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            nav_link(th, "nav-new", "New", route == Route::New).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    on_nav_new(this, Route::New, window, cx)
                                }),
                            ),
                        )
                        .when(has_schema, |el| {
                            el.child(
                                nav_link(th, "nav-view", "View", route == Route::View).on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        on_nav_view(this, Route::View, window, cx)
                                    }),
                                ),
                            )
                        })
                        .child(
                            nav_link(th, "nav-about", "About", route == Route::About).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    on_nav(this, Route::About, window, cx)
                                }),
                            ),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .when_some(update_badge, |el, b| el.child(b))
                .child(
                    div()
                        .id("theme-toggle")
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .h(px(32.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded_md()
                        .border_1()
                        .border_color(th.card_border)
                        .px(px(10.0))
                        .text_sm()
                        .text_color(th.text)
                        .cursor_pointer()
                        .hover(|el| el.bg(th.hover_bg))
                        .on_click(cx.listener(move |this, _, window, cx| on_theme(this, window, cx)))
                        .child(icon(theme_icon, px(16.0), th.text))
                        .child(SharedString::from(theme_label)),
                ),
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
