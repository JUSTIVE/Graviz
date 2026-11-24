//! The ⌘K palette: search the schema from anywhere and jump to a hit.

use graviz_core::search::{SearchResult, search_graph};
use nsh_core::prelude::*;
use nsh_metro::{Icon, theme::{LIGHT, SEMIBOLD}};

use crate::{
    config,
    store::Store,
    tree::highlighted,
    ui::{MONO, c, icon, kind_label, metro, mono, th},
};

const ROW_H: f32 = 52.0;
const PAD: f32 = 16.0;
const VISIBLE_ROWS: f32 = 8.0;

pub fn page(store: Store) -> impl IntoElement {
    let query = signal(std::env::var("GRAVIZ_PALETTE").unwrap_or_default());
    let active = signal(0usize);
    let focused = signal(true);
    let input_ref = element_ref();
    // Focus once it is on screen; `auto_focus` alone loses to the canvas
    // holding the window's attention.
    let _ = set_timeout(std::time::Duration::from_millis(30), move || input_ref.focus());
    // Each opening starts empty.
    effect(move || {
        let generation = store.palette_gen.get();
        if generation > 0 {
            untracked(|| {
                query.set(String::new());
                active.set(0);
            });
        }
    });
    let hits = move || -> Vec<SearchResult> {
        store.model.with(|m| m.as_ref().map(|m| search_graph(&m.graph, &query.get())).unwrap_or_default())
    };
    let choose = move |ix: usize| {
        let Some(r) = hits().into_iter().nth(ix) else { return };
        let q = query.get_untracked();
        if !q.trim().is_empty() {
            config::push_search(&q);
        }
        store.palette_open.set(false);
        store.navigate_to(r.node_index as u32, r.row_index);
    };
    crate::ui::arrow_keys(move |d| {
        let n = untracked(|| hits().len());
        active.update(|a| *a = (*a as i32 + d).clamp(0, n.saturating_sub(1) as i32) as usize);
    });
    view()
        .absolute()
        .inset_0()
        .flex_row()
        .justify_center()
        .items_start()
        .bg(c(|t| t.bg.opacity(0.6)))
        .z_index(50)
        .on_press(move || store.palette_open.set(false))
        .child(
            view()
                .mt(px(72.0))
                .w(px(640.0))
                .max_h(px(ROW_H * VISIBLE_ROWS + 132.0))
                .pt(px(8.0))
                .pb(px(8.0))
                .bg(c(|t| t.chrome_bg))
                // Presses inside stay inside.
                .on_press(|| {})
                .child(metro("search", 40.0).flex_none().px(px(PAD)).line_height(px(48.0)).font_weight(LIGHT).text_color(c(|t| t.text)))
                .child(
                    view()
                        .flex_none()
                        .h(px(40.0))
                        .mx(px(PAD))
                        .mt(px(6.0))
                        .mb(px(8.0))
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(10.0))
                        .border_2()
                        .border_color(move || if focused.get() { th().accent.hsla() } else { th().card_border.hsla() })
                        .bg(c(|t| t.input_bg))
                        .child(icon(Icon::Search, 15.0, c(|t| t.text_muted)))
                        .child(
                            nsh_core::element::input()
                                .element_ref(&input_ref)
                                .flex_1()
                                .font_family(MONO)
                                .text_size(px(14.0))
                                .text_color(c(|t| t.text))
                                .placeholder("Search types & fields…")
                                .placeholder_color(c(|t| t.text_muted))
                                .caret_color(c(|t| t.accent))
                                .auto_focus(true)
                                .value(move || query.get())
                                .on_change(move |s: &str| {
                                    query.set(s.to_string());
                                    active.set(0);
                                })
                                .on_focus(move || focused.set(true))
                                .on_blur(move || focused.set(false))
                                .on_submit(move |_: &str| choose(active.get_untracked()))
                                .on_key_down(move |e: &KeyEvent| match e.key.as_ref() {
                                    "Escape" => store.palette_open.set(false),
                                    _ => {}
                                }),
                        )
                        .child(mono("esc", 10.0).flex_none().text_color(c(|t| t.text_faint))),
                )
                .child(dynamic(move || {
                    let results = hits();
                    if results.is_empty() {
                        let empty = query.with(|q| q.trim().is_empty());
                        return metro(if empty { "type to search the schema" } else { "no matches" }, 14.0)
                            .px(px(PAD))
                            .py(px(12.0))
                            .text_color(c(|t| t.text_muted))
                            .into_element();
                    }
                    let mut list = view();
                    for (ix, r) in results.iter().enumerate() {
                        list = list.child(row(ix, r, active, choose));
                    }
                    scroll().max_h(px(ROW_H * VISIBLE_ROWS)).child(list).into_element()
                })),
        )
}

fn row(ix: usize, r: &SearchResult, active: Signal<usize>, choose: impl Fn(usize) + Copy + 'static) -> impl IntoElement {
    let is_active = move || active.get() == ix;
    let primary = c(|t| t.accent);
    let name = match &r.field_name {
        Some(field) => text("")
            .child(highlighted(&r.type_name, r.type_match_indices.as_deref().unwrap_or(&[]), primary).text_color(c(|t| t.text_muted)))
            .child(text(".").text_color(c(|t| t.text_muted)))
            .child(highlighted(field, &r.match_indices, primary).text_color(c(|t| t.text))),
        None => highlighted(&r.type_name, &r.match_indices, primary).text_color(c(|t| t.text)),
    };
    let kind = r.type_kind;
    let subtitle = r.field_type.clone().or_else(|| r.snippet.as_ref().map(|s| s.snippet.clone()));
    view()
        .w_full()
        .h(px(ROW_H))
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .px(px(PAD))
        .cursor(Cursor::Pointer)
        .bg(move || if is_active() { th().active_bg.hsla() } else { crate::theme::Color::CLEAR.hsla() })
        .hover(|s| s.bg(th().hover_bg.hsla()))
        .on_press(move || choose(ix))
        .child(
            view()
                .flex_none()
                .size(px(40.0))
                .justify_end()
                .p(px(3.0))
                .bg(move || th().kind_color(kind).hsla())
                .child(text(kind_label(kind)).text_size(px(9.0)).font_weight(SEMIBOLD).text_color(rgb(0xffffff))),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.))
                .child(name.font_family(MONO).text_size(px(16.0)).line_height(px(22.0)).truncate())
                .child(when(subtitle.is_some(), move || {
                    metro(subtitle.clone().unwrap_or_default(), 12.0)
                        .w_full()
                        .truncate()
                        .line_height(px(16.0))
                        .text_color(move || if is_active() { th().accent.hsla() } else { th().text_muted.hsla() })
                })),
        )
}
