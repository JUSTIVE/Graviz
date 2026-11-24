//! The title strip and the nav pivot along the top of every screen.

use nsh_core::prelude::*;
use nsh_metro::theme::{LIGHT, SEMIBOLD};

use crate::{
    store::{Route, Store},
    ui::{c, file_label, metro, MONO},
};

/// The traffic lights' width, so the wordmark clears them.
const CONTROLS_INSET: f32 = 78.0;
pub const TITLEBAR_H: f32 = 28.0;
const NAV_SIZE: f32 = 30.0;
const NAV_MARGIN: f32 = 18.0;

/// The commit the binary was built from, when the build stamped one.
pub const COMMIT: Option<&str> = option_env!("GRAVIZ_COMMIT");

/// The wordmark, the open file and its directory, and the update link.
pub fn header(store: Store) -> impl IntoElement {
    view()
        .flex_none()
        .h(px(TITLEBAR_H))
        .w_full()
        .relative()
        .bg(c(|t| t.bg))
        // Behind everything: the strip a drag moves and a double click zooms.
        .child(native("graviz.titlestrip").absolute().inset_0())
        .child(
            view()
                .absolute()
                .inset_0()
                .flex_row()
                .items_center()
                .gap(px(12.0))
                .pl(px(CONTROLS_INSET))
                .pr(px(NAV_MARGIN))
                .pointer_events(PointerEvents::BoxNone)
                .child(metro("GRAVIZ", 12.0).font_weight(SEMIBOLD).text_color(c(|t| t.text)))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.))
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .overflow_hidden()
                        .child(dynamic(move || {
                            let Some(doc) = store.doc.get() else { return view().into_element() };
                            let (name, dir) = file_label(&doc.path);
                            view()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .min_w(px(0.))
                                .child(
                                    metro(name, 12.0)
                                        .max_w(px(280.0))
                                        .truncate()
                                        .text_color(c(|t| t.text_muted)),
                                )
                                .child(when(dir.is_some(), move || {
                                    metro(dir.clone().unwrap_or_default(), 12.0)
                                        .number_of_lines(1u32)
                                        .text_overflow(nsh_core::EllipsizeMode::Head)
                                        .min_w(px(0.))
                                        .text_color(c(|t| t.text_muted.opacity(0.7)))
                                }))
                                .into_element()
                        })),
                )
                .child(dynamic(move || match store.update.get() {
                    Some((version, url)) => view()
                        .pointer_events(PointerEvents::Auto)
                        .child(
                            metro(format!("v{version} available"), 12.0)
                                .text_color(c(|t| t.accent))
                                .cursor(Cursor::Pointer)
                                .on_press(move || {
                                    let _ = open_url(url.clone());
                                }),
                        )
                        .into_element(),
                    None => view().into_element(),
                })),
        )
}

/// The nav pivot: graph · new · settings · about, the current one in the
/// foreground. "graph" only once a schema is open.
pub fn nav(store: Store) -> impl IntoElement {
    let item = move |route: Option<Route>, label: &'static str| {
        let active = move || match route {
            Some(r) => !store.settings_open.get() && store.route.get() == r,
            None => store.settings_open.get(),
        };
        metro(label, NAV_SIZE)
            .line_height(px(NAV_SIZE * 1.2))
            .font_weight(LIGHT)
            .text_color(move || {
                let t = crate::ui::th();
                if active() { t.text.hsla() } else { t.text_muted.hsla() }
            })
            .hover(|s| s.opacity(1.0))
            .cursor(Cursor::Pointer)
            .on_press(move || match route {
                Some(r) => {
                    store.settings_open.set(false);
                    store.route.set(r);
                }
                None => store.settings_open.set(true),
            })
    };
    view()
        .flex_none()
        .w_full()
        .flex_row()
        .items_end()
        .gap(px(22.0))
        .px(px(NAV_MARGIN))
        .pt(px(2.0))
        .pb(px(8.0))
        .overflow_hidden()
        .bg(c(|t| t.bg))
        .child(when(move || store.has_schema(), move || item(Some(Route::View), "graph")))
        .child(item(Some(Route::New), "new"))
        .child(item(None, "settings").tooltip(text("Settings (⌘,)").into_element()))
        .child(item(Some(Route::About), "about"))
}

/// The build's commit, centred along the bottom, when there is one.
pub fn commit_badge() -> impl IntoElement {
    when(COMMIT.is_some(), || {
        view()
            .absolute()
            .bottom(px(8.0))
            .left(px(0.))
            .right(px(0.))
            .flex_row()
            .justify_center()
            .pointer_events(PointerEvents::None)
            .child(text(COMMIT.unwrap_or_default()).font_family(MONO).text_size(px(10.0)).text_color(c(|t| t.text_muted.opacity(0.4))))
    })
}
