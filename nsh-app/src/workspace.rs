//! The graph workspace: the sidebar's pivot of tabs, the canvas with its
//! floating cards, and the overlay dock along the bottom.

use std::time::Duration;

use graviz_core::graph::NodeKind;
use nsh_core::prelude::*;
use nsh_metro::{Icon, theme::{LIGHT, SEMIBOLD}};

use crate::{
    config::{self, DOCK_MAX_H, DOCK_MIN_H, SIDEBAR_MAX_W, SIDEBAR_MIN_W},
    graph::canvas::HistoryItem,
    loader,
    model::{EdgeGroup, Mode, RowKind},
    palette, panels,
    store::{Route, Store},
    theme::Color,
    tree,
    ui::{Glyph, bar_button, c, glyph, icon, kind_badge, metro, mono, small_check, th},
};

/// How long the sidebar takes to open or close.
const SIDEBAR_MS: f32 = 300.0;

/// Hot reload: the schema file is checked once a second.
pub fn watch_schema(store: Store) {
    let last = stored(0u128);
    let _ = set_interval(Duration::from_secs(1), move || {
        let Some(doc) = store.doc.get_untracked() else { return };
        if doc.path.as_os_str() == "(pasted)" {
            return;
        }
        let cur = loader::fingerprint(&doc.path);
        let prev = last.get();
        if prev != 0 && cur != prev && store.reload_from_disk() {
            store.notify("schema reloaded", doc.name.to_string());
        }
        last.replace(cur);
    });
}

pub fn page(store: Store) -> impl IntoElement {
    // Debug: GRAVIZ_FOCUS=<TypeName> opens with that card focused.
    if let Ok(name) = std::env::var("GRAVIZ_FOCUS") {
        let _ = set_timeout(Duration::from_millis(300), move || store.navigate_to_type(&name));
    }
    let sidebar_open = move || store.settings.with(|s| s.sidebar_open);
    let sidebar_w = move || store.settings.with(|s| s.sidebar_width.clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W));
    // Anything that assumes the sidebar is gone waits out its collapse.
    let settled = signal(true);
    effect(move || {
        let _ = sidebar_open();
        settled.set(false);
        let _ = set_timeout(Duration::from_millis(350), move || settled.set(true));
    });
    let inset = move || if !sidebar_open() && settled.get() { 44.0 } else { 0.0 };

    view()
        .absolute()
        .inset_0()
        .flex_row()
        .child(sidebar(store, sidebar_open, sidebar_w))
        .child(when(sidebar_open, move || splitter_v(store)))
        .child(
            view()
                .flex_1()
                .h_full()
                .min_w(px(0.))
                .child(
                    view()
                        .flex_1()
                        .min_h(px(0.))
                        .relative()
                        .child(graph_view(store))
                        .child(view_controls(store, inset))
                        .child(investigate_card(store, inset))
                        .child(recent_panel(store))
                        .child(when(move || !sidebar_open() && settled.get(), move || expand_button(store)))
                        .child(canvas_overlays(store))
                        .child(dynamic(move || match store.toast.get() {
                            Some((_, title, message)) => nsh_metro::toast(title)
                                .message(message)
                                .icon(Icon::Refresh)
                                .on_click(move || store.toast.set(None))
                                .into_element(),
                            None => view().into_element(),
                        })),
                )
                .child(dock(store)),
        )
        .child(when(move || store.palette_open.get(), move || palette::page(store)))
}

/* ---------------- sidebar ---------------- */

fn sidebar(store: Store, open: impl Fn() -> bool + Copy + 'static, width: impl Fn() -> f32 + Copy + 'static) -> impl IntoElement {
    // The modes as a Metro pivot: the selected header leads, the rest follow
    // it, a swipe or a tap on a header changes pane. A mode with nothing in
    // it has no header (gpui showed it faded and unclickable).
    let body = dynamic(move || {
        let Some(doc) = store.doc.get() else { return view().into_element() };
        let mut modes = vec![(Mode::Reachable, None)];
        for (mode, count) in [(Mode::Orphaned, doc.orphan_count), (Mode::Deprecated, doc.deprecated_count)] {
            if count > 0 {
                modes.push((mode, Some(count)));
            }
        }
        let headers: Vec<String> =
            modes.iter().map(|&(m, n)| format!("{}{}", mode_word(m), n.map(superscript).unwrap_or_default())).collect();
        let list: Vec<Mode> = modes.iter().map(|&(m, _)| m).collect();
        let at = {
            let list = list.clone();
            move || list.iter().position(|m| *m == store.mode.get()).unwrap_or(0)
        };
        let pick = list.clone();
        nsh_metro::pivot(headers, at)
            .panes(list.into_iter().map(|mode| {
                move || match mode {
                    Mode::Reachable => tree::page(store).into_element(),
                    Mode::Orphaned => panels::orphans(store).into_element(),
                    Mode::Deprecated => panels::deprecated(store).into_element(),
                }
            }))
            .on_select(move |ix| {
                if let Some(&mode) = pick.get(ix) {
                    store.set_mode(mode);
                }
            })
            .into_element()
    });
    view()
        .h_full()
        .flex_none()
        .overflow_hidden()
        .bg(c(|t| t.panel))
        .w(move || px(if open() { width() } else { 0.0 }))
        .transition_all()
        .duration(SIDEBAR_MS)
        .ease_out()
        .child(
            view()
                .w(move || px(width()))
                .h_full()
                .flex_none()
                .relative()
                .child(body)
                // Over the end of the header row, which runs off the column.
                .child(
                    view()
                        .absolute()
                        .top(px(18.0))
                        .right(px(6.0))
                        .size(px(36.0))
                        .items_center()
                        .justify_center()
                        .bg(c(|t| t.panel))
                        .cursor(Cursor::Pointer)
                        .hover(|s| s.bg(th().hover_bg.hsla()))
                        .on_press(move || store.set_settings(|s| s.sidebar_open = false))
                        .child(glyph(Glyph::PanelLeftClose, 16.0, c(|t| t.text_muted))),
                ),
        )
}

fn mode_word(mode: Mode) -> &'static str {
    match mode {
        Mode::Reachable => "reachable",
        Mode::Orphaned => "orphaned",
        Mode::Deprecated => "deprecated",
    }
}

/// `14` as `¹⁴`, the count beside a pivot header.
fn superscript(n: usize) -> String {
    const DIGITS: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];
    n.to_string().chars().map(|d| DIGITS[d.to_digit(10).unwrap_or(0) as usize]).collect()
}

/// The sidebar's right edge, dragged to resize it.
fn splitter_v(store: Store) -> impl IntoElement {
    let start = stored(0.0f32);
    let dragging = signal(false);
    view()
        .flex_none()
        .w(px(5.0))
        .h_full()
        .cursor(Cursor::ColResize)
        .bg(move || if dragging.get() { th().accent.hsla() } else { th().panel_border.hsla() })
        .hover(|s| s.bg(th().accent.opacity(0.6).hsla()))
        .claim_touches(TouchClaim::on_start())
        .on_responder_grant(move |_: &ResponderEvent| {
            start.replace(store.settings.with_untracked(|s| s.sidebar_width));
            dragging.set(true);
        })
        .on_responder_move(move |e: &ResponderEvent| {
            let w = (start.get() + e.gesture.delta.x.0).clamp(SIDEBAR_MIN_W, SIDEBAR_MAX_W);
            store.settings.update(|s| s.sidebar_width = w);
        })
        .on_responder_release(move |_: &ResponderEvent| {
            dragging.set(false);
            config::save_settings(&store.settings.get_untracked());
        })
}

/// The dock's top edge, dragged to resize it.
fn splitter_h(store: Store) -> impl IntoElement {
    let start = stored(0.0f32);
    let dragging = signal(false);
    view()
        .flex_none()
        .h(px(5.0))
        .w_full()
        .cursor(Cursor::RowResize)
        .bg(move || if dragging.get() { th().accent.hsla() } else { th().panel_border.hsla() })
        .hover(|s| s.bg(th().accent.opacity(0.6).hsla()))
        .claim_touches(TouchClaim::on_start())
        .on_responder_grant(move |_: &ResponderEvent| {
            start.replace(store.settings.with_untracked(|s| s.dock_height));
            dragging.set(true);
        })
        .on_responder_move(move |e: &ResponderEvent| {
            let vh = window().height.0;
            let h = (start.get() - e.gesture.delta.y.0).clamp(DOCK_MIN_H, DOCK_MAX_H.min(vh * 0.7));
            store.settings.update(|s| s.dock_height = h);
        })
        .on_responder_release(move |_: &ResponderEvent| {
            dragging.set(false);
            config::save_settings(&store.settings.get_untracked());
        })
}

fn expand_button(store: Store) -> impl IntoElement {
    view()
        .absolute()
        .top(px(16.0))
        .left(px(16.0))
        .bg(c(|t| t.chrome_bg))
        .p(px(6.0))
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(th().hover_bg.hsla()))
        .on_press(move || store.set_settings(|s| s.sidebar_open = true))
        .child(glyph(Glyph::PanelLeftOpen, 16.0, c(|t| t.text_muted)))
}

/* ---------------- the canvas ---------------- */

fn graph_view(store: Store) -> impl IntoElement {
    native("graviz.graph")
        .element_ref(&store.graph_ref)
        .absolute()
        .inset_0()
        .prop("model", move || serde_json::json!([store.model_id.get(), false]))
        .prop("theme", move || {
            let t = nsh_metro::Theme::get();
            serde_json::json!({
                "dark": t.mode == nsh_metro::Mode::Dark,
                "accent": crate::theme::accent_hex(t.accent),
            })
        })
        .prop("investigate", move || serde_json::json!(store.investigate.get()))
        .prop("highlightOverlay", move || serde_json::json!(store.highlight_overlay.get()))
        .prop("scrollMode", move || {
            serde_json::json!(match store.scroll_mode() {
                config::ScrollMode::Zoom => "zoom",
                config::ScrollMode::Pan => "pan",
            })
        })
        .prop("reduceMotion", move || serde_json::json!(reduce_motion()))
        .on_native_event(move |e: &NativeEvent| store.on_canvas_event(&e.name, &e.data))
}

/// The web's "0.4 at rest, 1.0 on hover" fade on the floating cards.
fn hover_fade(el: View) -> View {
    let hovered = signal(false);
    el.opacity(move || if hovered.get() { 1.0_f32 } else { 0.4 })
        .transition_opacity()
        .duration(300.0)
        .on_mouse_enter(move |_: &MouseEvent| hovered.set(true))
        .on_mouse_leave(move |_: &MouseEvent| hovered.set(false))
}

fn view_controls(store: Store, inset: impl Fn() -> f32 + Copy + 'static) -> impl IntoElement {
    let s = store.settings;
    hover_fade(
        view()
            .absolute()
            .top(move || px(16.0 + inset()))
            .left(px(16.0))
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .bg(c(|t| t.chrome_bg))
            .p(px(6.0))
            .child(small_check(move || s.with(|s| s.hide_primitive_fields), "hide primitives", move |_| {
                store.toggle_option(|s| s.hide_primitive_fields = !s.hide_primitive_fields)
            }))
            .child(small_check(move || s.with(|s| s.hide_relay), "hide relay", move |_| store.toggle_relay()))
            .child(small_check(move || s.with(|s| s.show_descriptions), "show descriptions", move |_| {
                store.toggle_option(|s| s.show_descriptions = !s.show_descriptions)
            }))
            .child(small_check(move || s.with(|s| s.bundle_edges), "bundle edges", move |_| {
                store.toggle_option(|s| s.bundle_edges = !s.bundle_edges)
            }))
            .child(small_check(move || s.with(|s| s.hide_custom_scalars), "hide custom scalars", move |_| {
                store.toggle_option(|s| s.hide_custom_scalars = !s.hide_custom_scalars)
            })),
    )
}

fn investigate_card(store: Store, inset: impl Fn() -> f32 + Copy + 'static) -> impl IntoElement {
    let coverage = move || {
        store.model.with(|m| {
            let (documented, total) = m.as_ref().map_or((0, 0), |m| m.desc_coverage);
            if total > 0 { documented as f32 / total as f32 } else { 1.0 }
        })
    };
    hover_fade(
        view()
            .absolute()
            .top(move || px(56.0 + inset()))
            .left(px(16.0))
            .gap(px(4.0))
            .bg(c(|t| t.chrome_bg))
            .p(px(6.0))
            .child(metro("investigate", 16.0).line_height(px(18.0)).font_weight(LIGHT).text_color(c(|t| t.accent)))
            .child(
                view()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(small_check(move || store.investigate.get(), "missing descriptions", move |on| store.investigate.set(on)))
                    .child(
                        metro(move || format!("{}%", (coverage() * 100.0).round() as i32), 20.0)
                            .flex_none()
                            .line_height(px(24.0))
                            .font_weight(LIGHT)
                            .text_color(move || {
                                let (t, cov) = (th(), coverage());
                                if cov >= 0.9 {
                                    t.kind_color(NodeKind::Enum).hsla()
                                } else if cov >= 0.5 {
                                    t.type_amber.hsla()
                                } else {
                                    t.kind_color(NodeKind::Scalar).hsla()
                                }
                            }),
                    ),
            ),
    )
}

/// A history stop as the Recent list shows it.
fn recent_entry(store: Store, item: HistoryItem) -> Option<(NodeKind, &'static str, String)> {
    store.model.with_untracked(|m| {
        let m = m.as_ref()?;
        match item {
            HistoryItem::Card(c) => {
                let card = m.cards.get(c as usize)?;
                Some((card.kind, card.kind_label, card.name.to_string()))
            }
            HistoryItem::Edge(ei) => {
                let e = m.edges.get(ei as usize)?;
                let (from, to) = (m.cards.get(e.from as usize)?, m.cards.get(e.to as usize)?);
                Some((from.kind, from.kind_label, format!("{} → {}", from.name, to.name)))
            }
        }
    })
}

fn recent_panel(store: Store) -> impl IntoElement {
    dynamic(move || {
        let history = store.canvas.with(|c| c.history.clone());
        if history.is_empty() {
            return view().into_element();
        }
        let open = store.history_open.get();
        let count = history.len().min(50);
        let mut panel = view().absolute().top(px(16.0)).right(px(16.0)).w(px(256.0)).bg(c(|t| t.chrome_bg)).child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(6.0))
                .child(
                    view()
                        .flex_1()
                        .flex_row()
                        .items_start()
                        .gap(px(2.0))
                        .child(metro("recent", 18.0).font_weight(LIGHT).text_color(c(|t| t.text)))
                        .child(metro(count.to_string(), 10.0).mt(px(2.0)).font_weight(SEMIBOLD).text_color(c(|t| t.text_muted))),
                )
                .child(
                    view()
                        .p(px(4.0))
                        .cursor(Cursor::Pointer)
                        .hover(|s| s.bg(th().hover_bg.hsla()))
                        .on_press(move || store.command("clearHistory", serde_json::Value::Null))
                        .child(icon(Icon::Delete, 12.0, c(|t| t.text_muted))),
                )
                .child(
                    view()
                        .p(px(4.0))
                        .cursor(Cursor::Pointer)
                        .hover(|s| s.bg(th().hover_bg.hsla()))
                        .on_press(move || store.history_open.update(|o| *o = !*o))
                        .child(glyph(if open { Glyph::ChevronUp } else { Glyph::ChevronDown }, 12.0, c(|t| t.text_muted))),
                ),
        );
        if open {
            let mut list = view().py(px(4.0));
            for item in history.into_iter().rev().take(50) {
                let Some((kind, kind_label, label)) = recent_entry(store, item) else { continue };
                list = list.child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(8.0))
                        .py(px(2.0))
                        .child(
                            view()
                                .flex_row()
                                .flex_1()
                                .min_w(px(0.))
                                .items_center()
                                .gap(px(8.0))
                                .px(px(8.0))
                                .py(px(4.0))
                                .cursor(Cursor::Pointer)
                                .bg(move || th().kind_color(kind).opacity(0.1).hsla())
                                .hover(|s| s.bg(th().hover_bg.hsla()))
                                .on_press(move || store.command("revisit", item.to_json()))
                                .child(kind_badge(kind, kind_label))
                                .child(mono(label, 12.0).flex_1().min_w(px(0.)).truncate().text_color(move || th().kind_color(kind).hsla())),
                        )
                        .child(
                            view()
                                .p(px(4.0))
                                .cursor(Cursor::Pointer)
                                .hover(|s| s.bg(th().hover_bg.hsla()))
                                .on_press(move || store.command("removeHistory", item.to_json()))
                                .child(icon(Icon::Close, 12.0, c(|t| t.text_muted))),
                        ),
                );
            }
            // 60% of the window, matching the web.
            panel = panel.child(scroll().max_h(move || px(window().height.0 * 0.6)).child(list));
        }
        hover_fade(panel).into_element()
    })
}

/* ---------------- what the canvas shows around itself ---------------- */

/// The tooltip, the pinned edge's card, the perf panel, the back-to-graph
/// button and the right-click menu.
fn canvas_overlays(store: Store) -> impl IntoElement {
    view()
        .absolute()
        .inset_0()
        .pointer_events(PointerEvents::BoxNone)
        .child(perf_panel(store))
        .child(tooltip_view(store))
        .child(edge_card(store))
        .child(back_to_graph(store))
        .child(context_menu(store))
}

fn perf_panel(store: Store) -> impl IntoElement {
    let stats = move || {
        store.model.with(|m| {
            let Some(m) = m.as_ref() else { return String::new() };
            let mut s = format!("{} nodes · {} edges", m.cards.len(), m.edges.len());
            if m.overlay_marks > 0 {
                s.push_str(&format!(" · overlay +{}", m.overlay_marks));
            }
            if store.investigate.get() {
                let (documented, total) = m.desc_coverage;
                let pct = (documented * 100).checked_div(total).unwrap_or(100);
                s.push_str(&format!(" · desc {pct}%"));
            }
            s
        })
    };
    let chart = canvas(move || {
        let hist = store.perf.with(|p| p.hist.clone());
        let (w, h) = (260.0f32, 48.0f32);
        let mut p = Painter::new();
        if hist.is_empty() {
            return p;
        }
        let t = th();
        let peak = hist.iter().cloned().fold(65.0f32, f32::max);
        let max_fps = peak + 5.0;
        let bar_w = w / hist.len() as f32;
        for (i, v) in hist.iter().enumerate() {
            let bh = ((v / max_fps) * h).max(1.0);
            let color = if *v < peak * 0.5 { t.expired.opacity(0.7) } else { t.text_muted.opacity(0.35) };
            p = p.fill_color(color.hsla()).fill_rect(px(i as f32 * bar_w), px(h - bh), px((bar_w - 1.0).max(1.0)), px(bh));
        }
        p
    })
    .flex_none()
    .w(px(260.0))
    .h(px(48.0))
    .mb(px(6.0));
    view()
        .absolute()
        .bottom(px(16.0))
        .right(px(16.0))
        .min_w(px(280.0))
        .border_1()
        .border_color(c(|t| t.panel_border.opacity(0.2)))
        .bg(c(|t| t.bg.opacity(0.1)))
        .px(px(12.0))
        .py(px(8.0))
        .pointer_events(PointerEvents::None)
        .child(chart)
        .child(
            view()
                .flex_none()
                .flex_row()
                .items_end()
                .justify_between()
                .gap(px(16.0))
                .child(mono(move || format!("{} fps", store.perf.with(|p| p.fps.round() as i32)), 12.0).flex_none().text_color(c(|t| t.text_muted.opacity(0.6))))
                .child(mono(stats, 12.0).flex_none().text_color(c(|t| t.text_muted.opacity(0.6)))),
        )
        .child(
            mono(move || store.perf.with(|p| format!("paint {:.1}ms · {:.0}%", p.paint, p.k * 100.0)), 12.0)
                .mt(px(4.0))
                .opacity(0.7)
                .text_color(c(|t| t.text_muted.opacity(0.6))),
        )
}

/// The web's three tooltip shapes: field, header, edge.
struct Tip {
    badge: Option<(NodeKind, &'static str)>,
    title: String,
    type_text: Option<String>,
    description: Option<String>,
    deprecation: Option<String>,
    expired: bool,
    wide: bool,
}

fn tip_for(store: Store) -> Option<(Tip, f32, f32)> {
    let hover = store.hover.get();
    if hover.dragging {
        return None;
    }
    store.model.with(|m| {
        let m = m.as_ref()?;
        let tip = if let Some(card) = hover.card {
            let card = m.cards.get(card as usize)?;
            let badge = Some((card.kind, card.kind_label));
            match hover.row.as_ref().map(|(k, i)| (k.as_str(), *i)) {
                Some(("row", ri)) => {
                    let row = card.rows.get(ri)?;
                    let deprecation = row.deprecation_reason.clone().or_else(|| row.deprecated.then(|| "deprecated".to_string()));
                    if row.description.is_none() && deprecation.is_none() {
                        return None;
                    }
                    Tip {
                        badge,
                        title: format!("{}.{}", card.name, row.left),
                        type_text: (!row.right.is_empty()).then(|| row.right.to_string()),
                        description: row.description.clone(),
                        deprecation,
                        expired: row.until_expired,
                        wide: false,
                    }
                }
                Some(("implements", b)) => Tip {
                    badge,
                    title: format!("implements {}", card.implements.get(b)?),
                    type_text: None,
                    description: None,
                    deprecation: None,
                    expired: false,
                    wide: false,
                },
                Some(("union", b)) => Tip {
                    badge,
                    title: format!("member of union {}", card.member_of_unions.get(b)?),
                    type_text: None,
                    description: None,
                    deprecation: None,
                    expired: false,
                    wide: false,
                },
                // Header: only when the card no longer paints its name, or
                // it has a description to show.
                _ => {
                    if card.description.is_none() && hover.k >= crate::graph::LOD_ROWS {
                        return None;
                    }
                    Tip {
                        badge,
                        title: card.name.to_string(),
                        type_text: None,
                        description: card.description.clone(),
                        deprecation: None,
                        expired: false,
                        wide: false,
                    }
                }
            }
        } else {
            let e = m.edges.get(hover.edge? as usize)?;
            let from = &m.cards[e.from as usize];
            let to = &m.cards[e.to as usize].name;
            let shown: Vec<String> = e.labels.iter().take(10).map(|l| l.to_string()).collect();
            let more = e.labels.len().saturating_sub(10);
            let mut desc = shown.join(", ");
            if more > 0 {
                desc.push_str(&format!(" … +{more}"));
            }
            Tip {
                badge: Some((from.kind, from.kind_label)),
                title: format!("{} → {}", from.name, to),
                type_text: (e.bundled > 1).then(|| format!("{} fields", e.bundled)),
                description: (!desc.is_empty()).then_some(desc),
                deprecation: None,
                expired: false,
                wide: true,
            }
        };
        Some((tip, hover.x, hover.y))
    })
}

fn tooltip_view(store: Store) -> impl IntoElement {
    dynamic(move || {
        let Some((tip, hx, hy)) = tip_for(store) else { return view().into_element() };
        // Web placement: 12pt down-right of the cursor, flipping to the left
        // past the horizontal middle and upward within 80pt of the bottom.
        let tw = if tip.wide { 420.0 } else { 340.0 };
        let mut el = view()
            .absolute()
            .max_w(px(tw))
            .px(px(if tip.wide { 12.0 } else { 10.0 }))
            .py(px(if tip.wide { 8.0 } else { 6.0 }))
            .border_1()
            .border_color(c(|t| t.card_border))
            .bg(c(|t| t.chrome_bg))
            .gap(px(2.0))
            .pointer_events(PointerEvents::None)
            ;
        let cw = move || window().width.0;
        el = el
            .left(move || {
                let canvas_w = canvas_width(store, cw());
                px(if hx > canvas_w / 2.0 { (hx - tw - 12.0).max(4.0) } else { hx + 12.0 })
            })
            .top(move || {
                let vh = window().height.0 - 120.0;
                px(if hy > vh - 80.0 { (hy - 132.0).max(4.0) } else { hy + 12.0 })
            });
        let mut head = view().flex_row().flex_wrap().items_center().gap(px(6.0));
        if let Some((kind, label)) = tip.badge {
            head = head.child(
                view().flex_none().px(px(4.0)).bg(move || th().kind_color(kind).hsla()).child(mono(label, 9.0).text_color(rgb(0xffffff))),
            );
        }
        head = head.child(mono(tip.title, 12.5).font_weight(SEMIBOLD).text_color(c(|t| t.text)));
        if let Some(t) = tip.type_text {
            head = head.child(mono(t, 12.5).text_color(c(|t| t.type_amber)));
        }
        el = el.child(head);
        if let Some(d) = tip.description {
            el = el.child(mono(d, 12.5).line_height(px(20.0)).max_h(px(120.0)).overflow_hidden().text_color(c(|t| t.text_muted)));
        }
        if let Some(d) = tip.deprecation {
            let expired = tip.expired;
            el = el.child(
                mono(format!("⚠ {d}"), 11.0).text_color(move || if expired { th().expired.hsla() } else { th().type_amber.hsla() }),
            );
        }
        // gpui fades a tooltip in over 180ms.
        let fade = animated_value(if reduce_motion() { 1.0 } else { 0.0 });
        if !reduce_motion() {
            let _ = fade.timing(1.0, 180.0, Easing::EaseOut);
        }
        el.bind_opacity(&fade).into_element()
    })
}

/// The canvas's width: the window less the open sidebar.
fn canvas_width(store: Store, window_w: f32) -> f32 {
    let (open, w) = store.settings.with(|s| (s.sidebar_open, s.sidebar_width));
    window_w - if open { w + 5.0 } else { 0.0 }
}

/// The pinned edge, named at the bottom of the canvas: what tells you which
/// edge survived the dim, since a hover tooltip vanishes when you move.
fn edge_card(store: Store) -> impl IntoElement {
    dynamic(move || {
        let Some(ei) = store.canvas.with(|c| c.edge) else { return view().into_element() };
        let data = store.model.with(|m| {
            let m = m.as_ref()?;
            let e = m.edges.get(ei as usize)?;
            let (from, to) = (&m.cards[e.from as usize], &m.cards[e.to as usize]);
            let joiner = match e.group {
                EdgeGroup::Implements => Some("implements"),
                EdgeGroup::Union => Some("|"),
                _ => None,
            };
            let label = if e.bundled > 1 { Some(format!("({} fields)", e.bundled)) } else { e.labels.first().map(|l| l.to_string()) };
            Some((from.kind, from.kind_label, from.name.to_string(), to.kind, to.kind_label, to.name.to_string(), joiner, label, e.bundled > 1))
        });
        let Some((fk, fl, fname, tk, tl, tname, joiner, label, bundled)) = data else { return view().into_element() };
        let mut row = view()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .border_1()
            .border_color(c(|t| t.card_border))
            .bg(c(|t| t.chrome_bg))
            .px(px(12.0))
            .py(px(8.0))
            .pointer_events(PointerEvents::Auto)
            .child(kind_badge(fk, fl))
            .child(mono(fname, 12.0).font_weight(SEMIBOLD).text_color(c(|t| t.text)));
        if let Some(l) = label {
            row = row.child(mono(l, 12.0).text_color(move || if bundled { th().text_muted.hsla() } else { th().type_amber.hsla() }));
        }
        if let Some(j) = joiner {
            row = row.child(mono(j, 12.0).text_color(c(|t| t.text_muted)));
        }
        row = row
            .child(mono("→", 12.0).text_color(c(|t| t.text_muted)))
            .child(kind_badge(tk, tl))
            .child(mono(tname, 12.0).font_weight(SEMIBOLD).text_color(c(|t| t.text)))
            .child(
                view()
                    .ml(px(8.0))
                    .p(px(4.0))
                    .cursor(Cursor::Pointer)
                    .hover(|s| s.bg(th().hover_bg.hsla()))
                    .on_press(move || store.command("clearEdge", serde_json::Value::Null))
                    .child(icon(Icon::Close, 12.0, c(|t| t.text_muted))),
            );
        view()
            .absolute()
            .bottom(px(16.0))
            .left(px(0.))
            .right(px(0.))
            .flex_row()
            .justify_center()
            .pointer_events(PointerEvents::BoxNone)
            .child(row)
            .into_element()
    })
}

/// Once the whole graph has left the canvas, a way back to it, dead centre:
/// there is nothing behind it to cover.
fn back_to_graph(store: Store) -> impl IntoElement {
    when(move || store.canvas.with(|c| c.offscreen), move || {
        view()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .pointer_events(PointerEvents::BoxNone)
            .child(
                view()
                    .border_1()
                    .border_color(c(|t| t.card_border))
                    .bg(c(|t| t.chrome_bg))
                    .px(px(12.0))
                    .py(px(8.0))
                    .cursor(Cursor::Pointer)
                    .hover(|s| s.bg(th().hover_bg.hsla()))
                    .pointer_events(PointerEvents::Auto)
                    .on_press(move || store.command("fit", serde_json::Value::Null))
                    .child(metro("Back to graph", 12.0).text_color(c(|t| t.text))),
            )
    })
}

fn context_menu(store: Store) -> impl IntoElement {
    dynamic(move || {
        let Some(menu) = store.menu.get() else { return view().into_element() };
        let Some((type_name, field)) = store.model.with_untracked(|m| {
            let m = m.as_ref()?;
            let card = m.cards.get(menu.card as usize)?;
            let field = menu.row.and_then(|r| card.rows.get(r)).filter(|r| r.kind != RowKind::EnumValue || true).map(|r| r.left.to_string());
            Some((card.name.to_string(), field))
        }) else {
            return view().into_element();
        };
        let item = |label: String, copy: String| {
            mono(label, 12.0)
                .px(px(12.0))
                .py(px(6.0))
                .text_color(c(|t| t.text))
                .hover(|s| s.bg(th().hover_bg.hsla()))
                .cursor(Cursor::Pointer)
                .on_press(move || {
                    set_clipboard(copy.clone());
                    store.command("closeMenu", serde_json::Value::Null);
                })
        };
        let mut el = view()
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .min_w(px(180.0))
            .border_1()
            .border_color(c(|t| t.card_border))
            .bg(c(|t| t.chrome_bg))
            .py(px(4.0))
            .pointer_events(PointerEvents::Auto);
        if let Some(f) = field {
            let path = format!("{type_name}.{f}");
            el = el.child(item(format!("Copy \"{path}\""), path));
        }
        el.child(item("Copy type name".into(), type_name)).into_element()
    })
}

/* ---------------- the overlay dock ---------------- */

/// `2 additions, 1 override`: the web's applied-summary wording.
fn change_label(counts: Option<(usize, usize, usize)>) -> String {
    let Some((a, c_, r)) = counts else { return "no change".into() };
    let plural = |n: usize, w: &str| if n == 1 { format!("{n} {w}") } else { format!("{n} {w}s") };
    let mut parts = Vec::new();
    if a > 0 {
        parts.push(plural(a, "addition"));
    }
    if c_ > 0 {
        parts.push(plural(c_, "override"));
    }
    if r > 0 {
        parts.push(plural(r, "removal"));
    }
    if parts.is_empty() { "no change".into() } else { parts.join(", ") }
}

fn counts(store: Store) -> Option<(usize, usize, usize)> {
    store.overlay.with(|o| {
        o.diff.as_ref().map(|d| {
            (d.added_types.len() + d.added_fields.len(), d.changed_fields.len(), d.removed_types.len() + d.removed_fields.len())
        })
    })
}

fn count_pills(store: Store) -> impl IntoElement {
    dynamic(move || {
        let pill = |label: String, color: fn(&crate::theme::Theme) -> Color| {
            view().px(px(5.0)).bg(move || color(&th()).hsla()).child(metro(label, 11.0).line_height(px(16.0)).font_weight(SEMIBOLD).text_color(rgb(0xffffff)))
        };
        let mut row = view().flex_row().items_center().gap(px(3.0));
        match counts(store) {
            Some((a, c_, r)) if a + c_ + r > 0 => {
                if a > 0 {
                    row = row.child(pill(format!("+{a}"), |t| t.overlay_green));
                }
                if c_ > 0 {
                    row = row.child(pill(format!("~{c_}"), |t| t.accent));
                }
                if r > 0 {
                    row = row.child(pill(format!("\u{2212}{r}"), |t| t.red));
                }
            }
            Some(_) => row = row.child(metro("no change", 11.0).text_color(c(|t| t.text_muted))),
            None => {}
        }
        row.into_element()
    })
}

fn dock(store: Store) -> impl IntoElement {
    let applied = move || store.overlay.with(|o| o.applied.is_some());
    let dirty = move || {
        let draft = store.overlay_draft.with(|d| d.trim().to_string());
        let applied = store.overlay.with(|o| o.applied.clone().unwrap_or_default());
        draft != applied.trim()
    };
    let can_highlight = move || counts(store).is_some_and(|(a, c_, _)| a + c_ > 0);
    // ⌘↵ applies while the dock is open.
    on_key("mod+enter", move |_| {
        if store.dock_open.get_untracked() && store.has_schema() && store.route.get_untracked() == Route::View {
            store.apply_overlay();
        }
    });
    let more = move |collapse: bool| {
        view()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(40.0))
            .h(px(28.0))
            .cursor(Cursor::Pointer)
            .on_press(move || store.dock_open.set(!collapse))
            .child(icon(Icon::More, 22.0, c(|t| t.text)))
    };
    let collapsed = move || {
        view()
            .w_full()
            .h_full()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .bg(c(|t| t.chrome_bg))
            .pl(px(12.0))
            .cursor(Cursor::Pointer)
            .on_press(move || store.dock_open.set(true))
            .child(glyph(Glyph::Layers, 14.0, c(|t| t.overlay_green)))
            .child(metro("overlay", 16.0).flex_none().font_weight(LIGHT).text_color(c(|t| t.text)))
            .child(view().flex_1().min_w(px(0.)).child(dynamic(move || {
                let (error, applied) = store.overlay.with(|o| (o.error.clone(), o.applied.is_some()));
                match (error, applied) {
                    (Some(e), _) => metro(format!("not applied — {e}"), 11.0).truncate().text_color(c(|t| t.red)).into_element(),
                    (None, true) => count_pills(store).into_element(),
                    (None, false) => metro("sketch SDL on top of this schema", 11.0).text_color(c(|t| t.text_muted)).into_element(),
                }
            })))
            .child(more(false))
    };
    let expanded = move || {
        view()
            .w_full()
            .h_full()
            .overflow_hidden()
            .bg(c(|t| t.bg))
            .child(splitter_h(store))
            .child(
                view()
                    .flex_none()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .bg(c(|t| t.chrome_bg))
                    .pl(px(12.0))
                    .py(px(6.0))
                    .child(glyph(Glyph::Layers, 16.0, c(|t| t.overlay_green)))
                    .child(metro("overlay", 22.0).flex_none().font_weight(LIGHT).text_color(c(|t| t.text)))
                    .child(count_pills(store))
                    .child(view().flex_1())
                    .child(
                        metro(move || if dirty() && applied() { "unapplied edits · ⌘↵".to_string() } else { "⌘↵".to_string() }, 11.0)
                            .flex_none()
                            .text_color(c(|t| t.text_muted)),
                    )
                    .child(bar_button(
                        Icon::Edit,
                        || "highlight".into(),
                        can_highlight,
                        move || can_highlight() && store.highlight_overlay.get(),
                        move || store.highlight_overlay.update(|h| *h = !*h),
                    ))
                    .child(bar_button(
                        Icon::Check,
                        move || if applied() && dirty() { "re-apply".into() } else { "apply".into() },
                        move || dirty() || !applied(),
                        move || dirty() || !applied(),
                        move || store.apply_overlay(),
                    ))
                    .child(bar_button(Icon::Delete, || "clear".into(), applied, || false, move || store.clear_overlay()))
                    .child(bar_button(Icon::Download, || "load file".into(), || true, || false, move || store.open_overlay_file()))
                    .child(more(true)),
            )
            .child(
                view().flex_1().min_h(px(0.)).p(px(8.0)).child(
                    view().flex_1().border_1().border_color(c(|t| t.card_border)).bg(c(|t| t.input_bg)).child(
                        crate::sdl::editor(
                            move || store.overlay_draft.get(),
                            move |s: &str| store.overlay_draft.set(s.to_string()),
                            "type User { newField: String }   ·   - Type.field removes   ·   ⌘↵ apply",
                            8.0,
                        ),
                    ),
                ),
            )
            .child(dynamic(move || {
                let (error, applied, diff) = store.overlay.with(|o| (o.error.clone(), o.applied.is_some(), o.diff.clone()));
                if let Some(e) = error {
                    return view()
                        .flex_none()
                        .bg(c(|t| t.red.opacity(0.1)))
                        .px(px(12.0))
                        .py(px(8.0))
                        .child(metro("OVERLAY NOT APPLIED", 10.0).font_weight(SEMIBOLD).text_color(c(|t| t.red)))
                        .child(mono(e, 10.0).text_color(c(|t| t.red.opacity(0.9))))
                        .child(metro("The graph is still showing the schema without the overlay.", 10.0).text_color(c(|t| t.text_muted)))
                        .into_element();
                }
                if !applied {
                    return view().into_element();
                }
                let mut col = view().gap(px(4.0)).child(
                    metro(format!("OVERLAY APPLIED ({})", change_label(counts(store))), 10.0).font_weight(SEMIBOLD).text_color(c(|t| t.overlay_green)),
                );
                if let Some(d) = diff {
                    let mut added = view().flex_row().flex_wrap().gap(px(4.0));
                    for name in d.added_types.iter().take(12).cloned() {
                        let t = name.clone();
                        added = added.child(
                            mono(name, 10.0)
                                .bg(c(|t| t.overlay_green.opacity(0.2)))
                                .px(px(6.0))
                                .py(px(1.0))
                                .text_color(c(|t| t.overlay_green))
                                .cursor(Cursor::Pointer)
                                .on_press(move || store.navigate_to_type(&t)),
                        );
                    }
                    let line = |items: Vec<String>, color: fn(&crate::theme::Theme) -> Color| {
                        let mut r = view().flex_row().flex_wrap().gap(px(8.0));
                        for s in items {
                            r = r.child(mono(s, 10.0).text_color(move || color(&th()).hsla()));
                        }
                        r
                    };
                    col = col
                        .child(added)
                        .child(line(d.added_fields.iter().take(12).map(|f| format!("+{f}")).collect(), |t| t.text_muted))
                        .child(line(d.changed_fields.iter().take(12).map(|f| format!("~{f}")).collect(), |t| t.accent.opacity(0.9)))
                        .child(line(
                            d.removed_types.iter().chain(d.removed_fields.iter()).take(12).map(|f| format!("\u{2212}{f}")).collect(),
                            |t| t.red.opacity(0.8),
                        ));
                }
                scroll()
                    .flex_none()
                    .max_h(move || px(store.settings.with(|s| s.dock_height) * 0.45))
                    .bg(c(|t| t.overlay_green.opacity(0.1)))
                    .child(view().px(px(12.0)).py(px(8.0)).child(col))
                    .into_element()
            }))
    };
    view()
        .flex_none()
        .w_full()
        .overflow_hidden()
        .h(move || px(if store.dock_open.get() { store.settings.with(|s| s.dock_height.clamp(DOCK_MIN_H, DOCK_MAX_H)) } else { 34.0 }))
        .transition_all()
        .duration(300.0)
        .ease_out()
        .child(dynamic(move || if store.dock_open.get() { expanded().into_element() } else { collapsed().into_element() }))
}
