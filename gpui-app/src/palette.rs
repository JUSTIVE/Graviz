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
//!
//! The sidebar searches the root it is showing. The palette searches every
//! root at once, since "find me a type" should not first need the right root
//! picked, and lists the hits under the root that reaches them, in Query,
//! Mutation, Subscription order. A type more than one root reaches is listed
//! once, under the first.

use crate::field::{self, FieldKey, MONO};
use crate::textedit::TextEdit;
use crate::theme::Theme;
use crate::tree::{highlighted, kind_label};
use crate::workspace::kind_badge;
use graviz_core::graph::ParsedGraph;
use graviz_core::search::{search_graph, SearchResult};
use gpui::{
    div, prelude::*, px, App, Context, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    point, MouseButton, ScrollHandle, SharedString, Window,
};
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

const FONT_PX: f32 = 14.0;
const LINE_H: f32 = 22.0;
const ROW_H: f32 = 44.0;
/// Rows on screen before the list scrolls.
const VISIBLE_ROWS: f32 = 8.0;
/// A root's group header.
const HEADER_H: f32 = 26.0;
/// The results list's top padding (`py_1`), before the first child.
const LIST_PAD: f32 = 4.0;

pub enum PaletteEvent {
    /// `node_index` indexes into the slice of the `root` it was found under;
    /// the workspace switches to that root first when it is not showing it.
    Select { root: Option<String>, node_index: usize, row: Option<usize> },
    Dismiss,
}

/// What one group searches: a root's slice of the schema, cut the same way
/// the canvas cuts it so hit indices line up with its cards. `root` is unset
/// for the single, ungrouped source outside the Reachable tab.
pub struct Source {
    pub root: Option<SharedString>,
    pub graph: ParsedGraph,
}

struct Hit {
    source: usize,
    result: SearchResult,
}

pub struct Palette {
    sources: Vec<Source>,
    query: TextEdit,
    results: Vec<Hit>,
    active: usize,
    focus: FocusHandle,
    scroll: ScrollHandle,
    origin: Rc<Cell<f32>>,
}

impl Palette {
    pub fn new(sources: Vec<Source>, cx: &mut Context<Self>) -> Self {
        Palette {
            sources,
            query: TextEdit::default(),
            results: Vec::new(),
            active: 0,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            origin: Rc::new(Cell::new(0.0)),
        }
    }

    /// A rebuilt graph invalidates every hit, which indexes into it.
    pub fn set_sources(&mut self, sources: Vec<Source>) {
        self.sources = sources;
        self.refresh();
    }

    /// Debug: GRAVIZ_PALETTE=<query> opens on the palette holding that
    /// search, so a selfshot can reproduce it.
    /// GRAVIZ_PALETTE_STEP=<n> then moves the active row down n times once
    /// the list has laid out, to show the pinned header mid-list.
    pub fn debug_query(&mut self, cx: &mut Context<Self>) -> bool {
        let Ok(q) = std::env::var("GRAVIZ_PALETTE") else { return false };
        self.query.set_text(q);
        self.refresh();
        let steps: usize = std::env::var("GRAVIZ_PALETTE_STEP")
            .ok()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        if steps > 0 {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                let _ = this.update(cx, |this: &mut Self, cx| {
                    for _ in 0..steps {
                        this.step(1);
                    }
                    cx.notify();
                });
            })
            .detach();
        }
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
        // Sources come in root order, so the first root to reach a hit keeps
        // it and the groups come out in that order too.
        let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
        self.results = self
            .sources
            .iter()
            .enumerate()
            .flat_map(|(source, src)| {
                search_graph(&src.graph, &self.query.text)
                    .into_iter()
                    .map(move |result| Hit { source, result })
            })
            .filter(|h| seen.insert((h.result.type_name.clone(), h.result.field_name.clone())))
            .collect();
        self.active = 0;
        self.scroll.set_offset(point(px(0.0), px(0.0)));
    }

    /// Whether result `ix` opens a new group, and so is drawn under a header.
    fn starts_group(&self, ix: usize) -> bool {
        self.sources[self.results[ix].source].root.is_some()
            && (ix == 0 || self.results[ix - 1].source != self.results[ix].source)
    }

    fn grouped(&self) -> bool {
        self.sources.iter().any(|s| s.root.is_some())
    }

    /// Top of result `ix` in the list's content, below every header above
    /// it. Rows and headers have fixed heights, so this is exact.
    fn row_top(&self, ix: usize) -> f32 {
        let headers = (0..=ix).filter(|&i| self.starts_group(i)).count();
        LIST_PAD + ix as f32 * ROW_H + headers as f32 * HEADER_H
    }

    /// How far the list is scrolled, as a positive distance.
    fn scroll_top(&self) -> f32 {
        let max = f32::from(self.scroll.max_offset().y).max(0.0);
        (-f32::from(self.scroll.offset().y)).clamp(0.0, max)
    }

    /// The header pinned to the top of the list: the group of the topmost
    /// row, with how far it is pushed up by the next group's header. None
    /// while the group's own header is still in view.
    fn sticky(&self) -> Option<(SharedString, f32)> {
        if !self.grouped() {
            return None;
        }
        let top = self.scroll_top();
        let starts: Vec<usize> = (0..self.results.len()).filter(|&i| self.starts_group(i)).collect();
        let header_top = |i: usize| self.row_top(i) - HEADER_H;
        let cur = starts.iter().rposition(|&i| header_top(i) <= top)?;
        if header_top(starts[cur]) >= top {
            return None;
        }
        let root = self.sources[self.results[starts[cur]].source].root.clone()?;
        let shift = starts
            .get(cur + 1)
            .map(|&next| (header_top(next) - top - HEADER_H).min(0.0))
            .unwrap_or(0.0);
        Some((root, shift))
    }

    /// Scroll just enough to show the active row, keeping it out from under
    /// the pinned header.
    fn reveal_active(&self) {
        let view = f32::from(self.scroll.bounds().size.height);
        if view <= 0.0 {
            return;
        }
        let top = self.scroll_top();
        let row = self.row_top(self.active);
        let cover = if self.grouped() { HEADER_H } else { 0.0 };
        let next = if row - cover < top {
            row - cover
        } else if row + ROW_H > top + view {
            row + ROW_H - view + LIST_PAD
        } else {
            return;
        };
        let max = f32::from(self.scroll.max_offset().y).max(0.0);
        self.scroll.set_offset(point(px(0.0), px(-next.clamp(0.0, max))));
    }

    fn choose(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(h) = self.results.get(ix) else { return };
        let root = self.sources[h.source].root.as_ref().map(|r| r.to_string());
        let (node_index, row) = (h.result.node_index, h.result.row_index);
        if !self.query.text.trim().is_empty() {
            crate::config::push_search(&self.query.text);
        }
        cx.emit(PaletteEvent::Select { root, node_index, row });
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
        self.reveal_active();
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
        let r = &self.results[ix].result;
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
        div()
            .id(("palette-row", ix))
            .w_full()
            .h(px(ROW_H))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(1.0))
            .px_3()
            .cursor_pointer()
            .when(active, |el| el.bg(th.active_bg))
            .when(!active, |el| el.hover(|el| el.bg(th.hover_bg)))
            .on_click(cx.listener(move |this, _, _, cx| this.choose(ix, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .font_family(MONO)
                    .text_size(px(13.0))
                    .child(kind_badge(th, r.type_kind, kind_label(r.type_kind)))
                    .child(name)
                    .child(div().flex_1())
                    .when_some(r.field_type.clone(), |el, ft| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(11.0))
                                .text_color(th.text_muted)
                                .child(SharedString::from(ft)),
                        )
                    }),
            )
            // Why a prose hit matched, when the name did not.
            .when_some(r.snippet.as_ref().map(|s| s.snippet.clone()), |el, text| {
                el.child(
                    div()
                        .w_full()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_size(px(11.0))
                        .text_color(th.text_faint)
                        .child(SharedString::from(text)),
                )
            })
            .into_any_element()
    }
}

fn group_header(th: Theme, root: Option<SharedString>) -> gpui::Div {
    div()
        .h(px(HEADER_H))
        .flex()
        .items_end()
        .px_3()
        .pb(px(4.0))
        .font_family(MONO)
        .text_size(px(10.0))
        .text_color(th.text_muted)
        .children(root)
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
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for ix in 0..self.results.len() {
            if self.starts_group(ix) {
                let root = self.sources[self.results[ix].source].root.clone();
                rows.push(group_header(th, root).into_any_element());
            }
            rows.push(self.row(ix, th, cx));
        }
        let empty = self.query.text.trim().is_empty();

        div()
            .size_full()
            .flex()
            .justify_center()
            // A dimmed backdrop says the rest of the window is not listening,
            // and gives the click that dismisses somewhere to land.
            .bg(gpui::black().opacity(0.35))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismiss)),
            )
            .child(
                div()
                    .id("palette")
                    .mt(px(96.0))
                    .w(px(640.0))
                    .max_h(px(ROW_H * VISIBLE_ROWS + 56.0))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .border_1()
                    .border_color(th.card_border)
                    .bg(th.panel)
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
                    .child(
                        div()
                            .flex_none()
                            .h(px(44.0))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .border_b_1()
                            .border_color(th.panel_border)
                            .child(crate::icons::icon(
                                crate::icons::Icon::Search,
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
                                window,
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
                            .px_3()
                            .py_3()
                            .text_size(px(12.0))
                            .text_color(th.text_faint)
                            .child(if empty {
                                "Type to search the schema."
                            } else {
                                "No matches."
                            })
                            .into_any_element()
                    } else {
                        // gpui has no `position: sticky`; the pinned header
                        // is drawn over the list's top edge instead, from
                        // the scroll offset this render sees.
                        let sticky = self.sticky().map(|(root, shift)| {
                            group_header(th, Some(root))
                                .id("palette-sticky")
                                .absolute()
                                .top(px(shift))
                                .left_0()
                                .right_0()
                                .bg(th.panel)
                                .occlude()
                        });
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("palette-results")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll)
                                    .py(px(LIST_PAD))
                                    .children(rows),
                            )
                            .children(sticky)
                            .into_any_element()
                    }),
            )
    }
}
