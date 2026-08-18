//! Sidebar: type list + fuzzy search (the web app's TreePanel).
//!
//! The search box is a minimal key-capture input (schema identifiers are
//! ASCII); results come from `gompass_core::search::search_graph`.

use crate::canvas::kind_color;
use crate::model::Model;
use gompass_core::search::{search_graph, SearchResult};
use gpui::{
    div, prelude::*, px, rgb, uniform_list, App, Context, EventEmitter, FocusHandle, Focusable,
    KeyDownEvent, ScrollStrategy, SharedString, UniformListScrollHandle, Window,
};
use std::rc::Rc;

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
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
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
            search_graph(&self.model.graph, &self.query)
        };
        self.active = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let event = if self.query.is_empty() {
            self.all_sorted.get(ix).map(|&card| TreeEvent::Select {
                node_index: card as usize,
                row: None,
            })
        } else {
            self.results.get(ix).map(|r| TreeEvent::Select {
                node_index: r.node_index,
                row: r.row_index,
            })
        };
        if let Some(event) = event {
            self.active = ix;
            cx.emit(event);
            cx.notify();
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

    fn render_row(&self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let is_active = ix == self.active;
        let (dot, main, detail): (gpui::Hsla, String, Option<String>) = if self.query.is_empty() {
            let card = &self.model.cards[self.all_sorted[ix] as usize];
            (kind_color(card.kind), card.name.clone(), None)
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
            (kind_color(r.type_kind), main, detail)
        };

        div()
            .id(ix)
            .px_2()
            .h(px(ROW_H))
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .when(is_active, |el| el.bg(rgb(0x2a3140)))
            .hover(|el| el.bg(rgb(0x232936)))
            .on_click(cx.listener(move |this, _, _, cx| this.select(ix, cx)))
            .child(div().size(px(7.0)).rounded_full().bg(dot).flex_none())
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0xdde1e8))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(SharedString::from(main)),
            )
            .when_some(detail, |el, d| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x778092))
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
            .bg(rgb(0x14171d))
            .border_r_1()
            .border_color(rgb(0x242a35))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .m_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(if focused { rgb(0x4c8dff) } else { rgb(0x2c3340) })
                    .bg(rgb(0x1a1e26))
                    .text_sm()
                    .font_family("Menlo")
                    .text_color(if self.query.is_empty() {
                        rgb(0x687083)
                    } else {
                        rgb(0xdde1e8)
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
                    .text_color(rgb(0x687083))
                    .child(SharedString::from(if self.query.is_empty() {
                        format!("All types · {count}")
                    } else {
                        format!("{count} results")
                    })),
            )
            .child(
                uniform_list(
                    "tree-items",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        range.map(|ix| this.render_row(ix, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .track_scroll(&self.scroll),
            )
    }
}
