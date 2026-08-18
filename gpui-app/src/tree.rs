//! Sidebar: type list + fuzzy search (the web app's TreePanel).
//!
//! The search box is a minimal key-capture input (schema identifiers are
//! ASCII); results come from `gompass_core::search::search_graph`.

use crate::model::Model;
use gompass_core::graph::{EdgeKind, NodeKind};
use gompass_core::search::{search_graph, SearchResult};
use crate::theme::Theme;
use gpui::{
    div, prelude::*, px, uniform_list, App, Context, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, ScrollStrategy, SharedString, UniformListScrollHandle, Window,
};
use std::collections::HashSet;
use std::rc::Rc;

const KIND_CHIPS: [(NodeKind, &str); 6] = [
    (NodeKind::Object, "type"),
    (NodeKind::Interface, "iface"),
    (NodeKind::Union, "union"),
    (NodeKind::Enum, "enum"),
    (NodeKind::Input, "input"),
    (NodeKind::Scalar, "scalar"),
];

pub enum TreeEvent {
    Select { node_index: usize, row: Option<usize> },
}

pub struct TreePanel {
    model: Rc<Model>,
    query: String,
    results: Vec<SearchResult>,
    /// Alphabetical card indices for the no-query "all types" list.
    all_sorted: Vec<u32>,
    active: usize,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    kind_filter: HashSet<NodeKind>,
    /// Card index of the type shown in the detail section.
    selected: Option<u32>,
    /// `(referencing card, "Type.field")` rows for the detail section.
    referenced_by: Vec<(u32, SharedString)>,
}

impl TreePanel {
    pub fn new(model: Rc<Model>, cx: &mut Context<Self>) -> Self {
        let mut all_sorted: Vec<u32> = (0..model.cards.len() as u32).collect();
        all_sorted.sort_by(|&a, &b| {
            model.cards[a as usize].name.cmp(&model.cards[b as usize].name)
        });
        Self {
            model,
            query: String::new(),
            results: Vec::new(),
            all_sorted,
            active: 0,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            kind_filter: HashSet::new(),
            selected: None,
            referenced_by: Vec::new(),
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Swap in a different slice of the schema (mode change).
    pub fn set_model(&mut self, model: Rc<Model>, cx: &mut Context<Self>) {
        let mut all_sorted: Vec<u32> = (0..model.cards.len() as u32).collect();
        all_sorted.sort_by(|&a, &b| {
            model.cards[a as usize].name.cmp(&model.cards[b as usize].name)
        });
        self.model = model;
        self.all_sorted = all_sorted;
        self.query.clear();
        self.selected = None;
        self.referenced_by.clear();
        self.refresh();
        cx.notify();
    }

    fn item_count(&self) -> usize {
        if self.query.is_empty() {
            self.all_sorted.len()
        } else {
            self.results.len()
        }
    }

    fn refresh(&mut self) {
        self.results = if self.query.is_empty() {
            Vec::new()
        } else {
            let mut results = search_graph(&self.model.graph, &self.query);
            if !self.kind_filter.is_empty() {
                results.retain(|r| self.kind_filter.contains(&r.type_kind));
            }
            results
        };
        self.active = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn toggle_kind(&mut self, kind: NodeKind, cx: &mut Context<Self>) {
        if !self.kind_filter.insert(kind) {
            self.kind_filter.remove(&kind);
        }
        self.refresh();
        cx.notify();
    }

    /// Recompute the detail section for `card` (incoming field references).
    fn select_card(&mut self, card: u32, row: Option<usize>, cx: &mut Context<Self>) {
        self.selected = Some(card);
        let id = &self.model.graph.nodes[card as usize].id;
        let mut refs: Vec<(u32, SharedString)> = self
            .model
            .graph
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Field && &e.target == id && &e.source != id)
            .filter_map(|e| {
                let src = *self.model.index_of.get(&e.source)?;
                let field = e.source_field.as_deref().unwrap_or("?");
                Some((src, SharedString::from(format!("{}.{}", e.source, field))))
            })
            .collect();
        refs.sort_by(|a, b| a.1.cmp(&b.1));
        refs.dedup_by(|a, b| a.1 == b.1);
        refs.truncate(200);
        self.referenced_by = refs;
        cx.emit(TreeEvent::Select { node_index: card as usize, row });
        cx.notify();
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let target = if self.query.is_empty() {
            self.all_sorted.get(ix).map(|&card| (card, None))
        } else {
            self.results
                .get(ix)
                .map(|r| (r.node_index as u32, r.row_index))
        };
        if let Some((card, row)) = target {
            self.active = ix;
            self.select_card(card, row, cx);
        }
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || ks.modifiers.control {
            return;
        }
        match ks.key.as_str() {
            "backspace" => {
                self.query.pop();
                self.refresh();
            }
            "escape" => {
                self.query.clear();
                self.refresh();
            }
            "up" => {
                self.active = self.active.saturating_sub(1);
                self.scroll.scroll_to_item(self.active, ScrollStrategy::Nearest);
            }
            "down" => {
                self.active = (self.active + 1).min(self.item_count().saturating_sub(1));
                self.scroll.scroll_to_item(self.active, ScrollStrategy::Nearest);
            }
            "enter" => {
                self.select(self.active, cx);
                return;
            }
            _ => {
                if let Some(ch) = ks.key_char.as_deref() {
                    if !ch.chars().any(|c| c.is_control()) {
                        self.query.push_str(ch);
                        self.refresh();
                    }
                } else {
                    return;
                }
            }
        }
        cx.notify();
    }

    fn render_row(&self, ix: usize, th: Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let is_active = ix == self.active;
        let (dot, main, detail): (gpui::Hsla, SharedString, Option<String>) = if self
            .query
            .is_empty()
        {
            let card = &self.model.cards[self.all_sorted[ix] as usize];
            let detail = card.description.as_ref().map(|d| {
                let one: String = d.split_whitespace().collect::<Vec<_>>().join(" ");
                one.chars().take(60).collect::<String>()
            });
            (th.kind_color(card.kind), card.name.clone(), detail)
        } else {
            let r = &self.results[ix];
            let main = match &r.field_name {
                Some(f) => format!("{}.{}", r.type_name, f),
                None => r.type_name.clone(),
            };
            let detail = r
                .snippet
                .as_ref()
                .map(|s| s.snippet.clone())
                .or_else(|| r.field_type.clone());
            (th.kind_color(r.type_kind), main.into(), detail)
        };

        div()
            .id(ix)
            .w_full()
            .px_2()
            .h(px(ROW_H))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .when(is_active, |el| el.bg(th.active_bg))
            .hover(|el| el.bg(th.hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
            .child(div().size(px(7.0)).rounded_full().bg(dot).flex_none())
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(th.text)
                    .whitespace_nowrap()
                    .child(SharedString::from(main)),
            )
            .when_some(detail, |el, d| {
                el.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(th.text_muted)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(SharedString::from(d)),
                )
            })
    }
}

const ROW_H: f32 = 26.0;

impl EventEmitter<TreeEvent> for TreePanel {}

impl Focusable for TreePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::theme(window.appearance());
        let focused = self.focus.is_focused(window);
        let count = self.item_count();
        let query_display: SharedString = if self.query.is_empty() {
            "Search types…  (⌘K)".into()
        } else {
            self.query.clone().into()
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(th.panel)
            .border_r_1()
            .border_color(th.panel_border)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .m_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(if focused { th.accent } else { th.card_border })
                    .bg(th.input_bg)
                    .text_sm()
                    .font_family("Menlo")
                    .text_color(if self.query.is_empty() {
                        th.text_faint
                    } else {
                        th.text
                    })
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(query_display),
            )
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .text_xs()
                    .text_color(th.text_faint)
                    .child(SharedString::from(if self.query.is_empty() {
                        format!("All types · {count}")
                    } else {
                        format!("{count} results")
                    })),
            )
            .when(!self.query.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .px_2()
                        .pb_1()
                        .children(KIND_CHIPS.map(|(kind, label)| {
                            let active = self.kind_filter.contains(&kind);
                            div()
                                .id(label)
                                .px_2()
                                .py(px(2.0))
                                .rounded_full()
                                .cursor_pointer()
                                .border_1()
                                .border_color(if active { th.kind_color(kind) } else { th.card_border })
                                .when(active, |el| el.bg(th.hover_bg))
                                .text_xs()
                                .text_color(th.kind_color(kind))
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle_kind(kind, cx)))
                                .child(SharedString::from(label))
                        })),
                )
            })
            .child(
                uniform_list(
                    "tree-items",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                        let th = crate::theme::theme(window.appearance());
                        range.map(|ix| this.render_row(ix, th, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .track_scroll(&self.scroll),
            )
            .when_some(self.selected, |el, card| {
                let c = &self.model.cards[card as usize];
                let refs = self.referenced_by.clone();
                let desc = c.description.clone();
                el.child(
                    div()
                        .flex_none()
                        .max_h(px(320.0))
                        .border_t_1()
                        .border_color(th.panel_border)
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .px_3()
                                .pt_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().size(px(7.0)).rounded_full().bg(th.kind_color(c.kind)).flex_none())
                                .child(
                                    div()
                                        .text_sm()
                                        .font_family("Menlo")
                                        .text_color(th.text)
                                        .child(c.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(th.kind_color(c.kind))
                                        .child(SharedString::from(c.kind_label)),
                                ),
                        )
                        .when_some(desc, |el, d| {
                            el.child(
                                div()
                                    .px_3()
                                    .pt_1()
                                    .text_xs()
                                    .text_color(th.text_muted)
                                    .max_h(px(80.0))
                                    .overflow_hidden()
                                    .child(SharedString::from(d)),
                            )
                        })
                        .child(
                            div()
                                .px_3()
                                .pt_2()
                                .pb_1()
                                .text_xs()
                                .text_color(th.text_faint)
                                .child(SharedString::from(format!(
                                    "Referenced by · {}",
                                    refs.len()
                                ))),
                        )
                        .child(
                            uniform_list(
                                "referenced-by",
                                refs.len(),
                                cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _w, cx| {
                                    range
                                        .map(|ix| {
                                            let (src, label) = this.referenced_by[ix].clone();
                                            div()
                                                .id(ix)
                                                .w_full()
                                                .px_3()
                                                .h(px(22.0))
                                                .flex()
                                                .items_center()
                                                .cursor_pointer()
                                                .hover(|el| el.bg(th.hover_bg))
                                                .text_xs()
                                                .font_family("Menlo")
                                                .text_color(th.text_muted)
                                                .whitespace_nowrap()
                                                .overflow_hidden()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.select_card(src, None, cx)
                                                }))
                                                .child(label)
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .h(px((refs.len() as f32 * 22.0).min(176.0))),
                        ),
                )
            })
    }
}
