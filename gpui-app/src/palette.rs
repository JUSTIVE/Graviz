//! The ⌘K palette: a search box over the whole schema, floating above
//! whatever you were looking at.
//!
//! ⌘K used to open the sidebar and put the caret in its search box, which
//! meant the shortcut's answer depended on a pane being there, moved the
//! layout under you, and left the results wedged into a 340px column. A
//! palette owes nothing to the layout: it opens over the canvas, takes the
//! keyboard, and closes again when it has sent you somewhere.
//!
//! The sidebar's search stays as it is. It is the one you leave open while
//! reading, with its kind chips and its history; this is the one you hit
//! mid-thought to jump.

use crate::field::{self, FieldKey, MONO};
use crate::model::Model;
use crate::textedit::TextEdit;
use crate::theme::Theme;
use crate::tree::{highlighted, kind_label};
use gpui::{
    div, prelude::*, px, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, ScrollHandle, SharedString, Window,
};
use gpui_metro::theme::{LIGHT, SEMIBOLD};
use graviz_core::search::{search_graph, SearchResult};
use std::cell::Cell;
use std::rc::Rc;

const FONT_PX: f32 = 14.0;
const LINE_H: f32 = 22.0;
const ROW_H: f32 = 52.0;
/// Left/right page margin inside the palette.
const PAD: f32 = 16.0;
/// Rows on screen before the list scrolls.
const VISIBLE_ROWS: f32 = 8.0;

pub enum PaletteEvent {
    Select { node_index: usize, row: Option<usize> },
    Dismiss,
}

pub struct Palette {
    model: Rc<Model>,
    query: TextEdit,
    results: Vec<SearchResult>,
    active: usize,
    focus: FocusHandle,
    scroll: ScrollHandle,
    origin: Rc<Cell<f32>>,
}

impl Palette {
    pub fn new(model: Rc<Model>, cx: &mut Context<Self>) -> Self {
        Palette {
            model,
            query: TextEdit::default(),
            results: Vec::new(),
            active: 0,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            origin: Rc::new(Cell::new(0.0)),
        }
    }

    /// A rebuilt graph invalidates every hit, which indexes into it.
    pub fn set_model(&mut self, model: Rc<Model>) {
        self.model = model;
        self.refresh();
    }

    /// Debug: GRAVIZ_PALETTE=<query> opens on the palette holding that
    /// search, so a selfshot can reproduce it.
    pub fn debug_query(&mut self) -> bool {
        let Ok(q) = std::env::var("GRAVIZ_PALETTE") else { return false };
        self.query.set_text(q);
        self.refresh();
        true
    }

    /// Open on an empty box. A palette that came back holding the last search
    /// would need clearing before it was useful, every time.
    pub fn reopen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.clear();
        self.refresh();
        self.reopen_keeping_query(window, cx);
    }

    /// Take the keyboard without touching what is in the box.
    pub fn reopen_keeping_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn refresh(&mut self) {
        self.results = search_graph(&self.model.graph, &self.query.text);
        self.active = 0;
        self.scroll.scroll_to_item(0);
    }

    fn choose(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(r) = self.results.get(ix) else { return };
        let (node_index, row) = (r.node_index, r.row_index);
        if !self.query.text.trim().is_empty() {
            crate::config::push_search(&self.query.text);
        }
        cx.emit(PaletteEvent::Select { node_index, row });
    }

    fn step(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let last = self.results.len() - 1;
        self.active = match delta {
            d if d < 0 => self.active.saturating_sub(1),
            _ => (self.active + 1).min(last),
        };
        self.scroll.scroll_to_item(self.active);
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match field::key(&mut self.query, ev, cx) {
            FieldKey::Edited => self.refresh(),
            FieldKey::Moved => {}
            FieldKey::Up => self.step(-1),
            FieldKey::Down => self.step(1),
            FieldKey::Enter => {
                self.choose(self.active, cx);
                return;
            }
            FieldKey::Escape => {
                cx.emit(PaletteEvent::Dismiss);
                return;
            }
            FieldKey::Ignored => return,
        }
        cx.notify();
    }

    fn row(&self, ix: usize, th: Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let r = &self.results[ix];
        let active = ix == self.active;
        let name = match &r.field_name {
            Some(field) => div()
                .flex()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(div().flex_none().text_color(th.text_muted).child(highlighted(
                    &r.type_name,
                    r.type_match_indices.as_deref().unwrap_or(&[]),
                    th.primary,
                )))
                .child(div().flex_none().text_color(th.text_muted).child("."))
                .child(div().flex_none().text_color(th.text).child(highlighted(
                    field,
                    &r.match_indices,
                    th.primary,
                ))),
            None => div().flex().min_w_0().whitespace_nowrap().overflow_hidden().child(
                div().flex_none().text_color(th.text).child(highlighted(
                    &r.type_name,
                    &r.match_indices,
                    th.primary,
                )),
            ),
        };
        // Metro list item: a square kind tile leading, the name large beside
        // it, the type (or why prose matched) subtle underneath.
        let tile = div()
            .flex_none()
            .size(px(40.0))
            .flex()
            .items_end()
            .p(px(3.0))
            .bg(th.kind_color(r.type_kind))
            .text_size(px(9.0))
            .font_weight(SEMIBOLD)
            .text_color(gpui::white())
            .child(kind_label(r.type_kind));
        let subtitle = r
            .field_type
            .clone()
            .or_else(|| r.snippet.as_ref().map(|s| s.snippet.clone()));
        div()
            .id(("palette-row", ix))
            .w_full()
            .h(px(ROW_H))
            .flex()
            .items_center()
            .gap(px(12.0))
            .px(px(PAD))
            .cursor_pointer()
            .when(active, |el| el.bg(th.active_bg))
            .when(!active, |el| el.hover(|el| el.bg(th.hover_bg)))
            .on_click(cx.listener(move |this, _, _, cx| this.choose(ix, cx)))
            .child(tile)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(16.0))
                            .line_height(px(22.0))
                            .child(name),
                    )
                    .when_some(subtitle, |el, text| {
                        el.child(
                            div()
                                .w_full()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_size(px(12.0))
                                .line_height(px(16.0))
                                .text_color(if active { th.accent } else { th.text_muted })
                                .child(SharedString::from(text)),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl Focusable for Palette {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        let focused = self.focus.is_focused(window);
        let rows: Vec<gpui::AnyElement> =
            (0..self.results.len()).map(|ix| self.row(ix, th, cx)).collect();
        let empty = self.query.text.trim().is_empty();

        div()
            .size_full()
            .flex()
            .justify_center()
            // A dimmed backdrop says the rest of the window is not listening,
            // and gives the click that dismisses somewhere to land.
            .bg(th.bg.opacity(0.6))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismiss)),
            )
            .child(
                div()
                    .id("palette")
                    .mt(px(72.0))
                    .w(px(640.0))
                    .max_h(px(ROW_H * VISIBLE_ROWS + 132.0))
                    .flex()
                    .flex_col()
                    .pt(px(8.0))
                    .pb(px(8.0))
                    .bg(th.chrome_bg)
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::on_key_down))
                    // escape is bound to ClearSelection app-wide, and gpui
                    // runs bindings before key handlers and stops there, so
                    // the key itself never reaches on_key_down. Claiming the
                    // action is the only way to hear it, and stopping there
                    // keeps the same press from also clearing the canvas
                    // selection underneath.
                    .on_action(cx.listener(|_, _: &crate::workspace::ClearSelection, _, cx| {
                        cx.emit(PaletteEvent::Dismiss);
                        cx.stop_propagation();
                    }))
                    // The backdrop's own press dismisses; a press inside must
                    // not travel up to it.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // The Metro search page: a big lowercase header, then a
                    // flat box that lights up in the accent when it has focus.
                    .child(
                        div()
                            .flex_none()
                            .px(px(PAD))
                            .text_size(px(40.0))
                            .line_height(px(48.0))
                            .font_weight(LIGHT)
                            .text_color(th.text)
                            .child("search"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .h(px(40.0))
                            .mx(px(PAD))
                            .mt(px(6.0))
                            .mb(px(8.0))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px(px(10.0))
                            .border_2()
                            .border_color(if focused { th.accent } else { th.card_border })
                            .bg(th.input_bg)
                            .child(gpui_metro::icon(
                                gpui_metro::Icon::Search,
                                px(15.0),
                                th.text_muted,
                            ))
                            .child(field::input(
                                field::InputProps {
                                    th,
                                    edit: &self.query,
                                    placeholder: "Search types & fields…",
                                    focused,
                                    font_px: FONT_PX,
                                    line_h: LINE_H,
                                    origin: self.origin.clone(),
                                },
                                cx,
                                |this, offset, cx| {
                                    this.query.move_cursor(offset, false);
                                    cx.notify();
                                },
                            ))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(MONO)
                                    .text_size(px(10.0))
                                    .text_color(th.text_faint)
                                    .child("esc"),
                            ),
                    )
                    .child(if rows.is_empty() {
                        div()
                            .px(px(PAD))
                            .py_3()
                            .text_size(px(14.0))
                            .text_color(th.text_muted)
                            .child(if empty {
                                "type to search the schema"
                            } else {
                                "no matches"
                            })
                            .into_any_element()
                    } else {
                        div()
                            .id("palette-results")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .children(rows)
                            .into_any_element()
                    }),
            )
    }
}
