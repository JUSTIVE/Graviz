//! Sidebar: the web app's `TreePanel`, section for section.
//!
//! Top to bottom: the search input, the recent-search list (which replaces
//! everything below it while the input is focused and empty), the kind-filter
//! chips + search results, then the browse tree — root operation picker,
//! collapsible "All types", the context sections (implemented by / members /
//! referenced by) and the `TypeDetail` pane.
//!
//! The search box is a key-capture input over a [`TextEdit`] buffer — it has
//! a real caret and selection, but no IME; results come from
//! `graviz_core::search::search_graph`.

use crate::icons::{icon, Icon};
use crate::model::{mono_w, Model, RowKind};
use crate::textedit::TextEdit;
use crate::theme::Theme;
use crate::workspace::kind_badge;
use graviz_core::graph::NodeKind;
use graviz_core::search::{search_graph, SearchResult, SnippetKind};
use gpui::{
    div, prelude::*, px, transparent_black, uniform_list, AnyElement, App, ClipboardItem, Context,
    EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle, Hsla, KeyDownEvent,
    MouseButton, ScrollHandle, SharedString, StyledText, Window,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// Kind filter chips, in the web's `KIND_STYLES` declaration order.
const KIND_ORDER: [(NodeKind, &str); 6] = [
    (NodeKind::Object, "type"),
    (NodeKind::Interface, "interface"),
    (NodeKind::Union, "union"),
    (NodeKind::Enum, "enum"),
    (NodeKind::Scalar, "scalar"),
    (NodeKind::Input, "input"),
];

/// Scalars that are never navigable (web `BUILTIN`).
const BUILTIN: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];

/// Row pitch and height cap of the capped type lists — the web's
/// `VLIST_ROW_H` and `max-h-48`.
const LIST_ROW_H: f32 = 24.0;
const LIST_MAX_H: f32 = 192.0;

const MONO: &str = "Menlo";

/// The search box is monospaced so the caret can be placed by measuring the
/// text to its left rather than round-tripping through the text system.
const SEARCH_FONT_PX: f32 = 12.0;
const SEARCH_LINE_H: f32 = 18.0;

fn kind_label(kind: NodeKind) -> &'static str {
    KIND_ORDER
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, l)| *l)
        .unwrap_or("type")
}

pub enum TreeEvent {
    Select { node_index: usize, row: Option<usize> },
    /// A root operation button was picked in the root selector.
    RootPicked(String),
}

pub struct TreePanel {
    model: Rc<Model>,
    /// The search box's buffer: text plus caret and selection.
    search: TextEdit,
    /// Unfiltered hits — the kind chips count over these.
    results: Vec<SearchResult>,
    /// Indices into `results` surviving `kind_filter`.
    filtered: Vec<usize>,
    kind_counts: HashMap<NodeKind, usize>,
    kind_filter: HashSet<NodeKind>,
    /// Alphabetical card indices for the "All types" list.
    all_sorted: Vec<u32>,
    /// Index into `filtered` for keyboard navigation.
    active: usize,
    focus: FocusHandle,
    /// Focus of the search input itself. Separate from `focus` (the panel
    /// root) so that clicking a tree row does not read as "the user is
    /// searching" and swap the tree for the recent-search list.
    search_focus: FocusHandle,
    results_scroll: ScrollHandle,
    search_history: Vec<String>,
    /// Card index of the type shown in `TypeDetail`.
    selected: Option<u32>,
    /// `(card, display row)` of the field highlighted on the canvas.
    pinned: Option<(u32, usize)>,
    all_types_open: bool,
    root_pick: Option<SharedString>,
    /// `(card, the other interfaces it implements)` — Interface selection only.
    implementers: Vec<(u32, Vec<SharedString>)>,
    /// `(card, the other unions it belongs to)` — Union selection only.
    union_members: Vec<(u32, Vec<SharedString>)>,
    /// `(card, ".field" names pointing at the selection)`.
    referenced_by: Vec<(u32, Vec<SharedString>)>,
    /// `(card, display row)` of the field whose right-click menu is open.
    context_menu_field: Option<(u32, usize)>,
    /// Left edge of the search text, recorded at paint time so a click can be
    /// mapped to a caret offset.
    search_origin: Rc<Cell<f32>>,
}

impl TreePanel {
    pub fn new(model: Rc<Model>, cx: &mut Context<Self>) -> Self {
        let all_sorted = sorted_cards(&model);
        let root_pick = first_root(&model);
        let mut this = Self {
            model,
            search: TextEdit::default(),
            results: Vec::new(),
            filtered: Vec::new(),
            kind_counts: HashMap::new(),
            kind_filter: HashSet::new(),
            all_sorted,
            active: 0,
            focus: cx.focus_handle(),
            search_focus: cx.focus_handle(),
            results_scroll: ScrollHandle::new(),
            search_history: crate::config::search_history(),
            selected: None,
            pinned: None,
            all_types_open: false,
            root_pick,
            implementers: Vec::new(),
            union_members: Vec::new(),
            referenced_by: Vec::new(),
            context_menu_field: None,
            search_origin: Rc::new(Cell::new(0.0)),
        };
        // Debug presets, matching GRAVIZ_MODE / GRAVIZ_VIEW: open the panel
        // on a query or a selected type so selfshots can verify both states.
        if let Ok(q) = std::env::var("GRAVIZ_TREE") {
            this.search.set_text(q);
            this.refresh();
        }
        if let Ok(name) = std::env::var("GRAVIZ_TREE_SEL") {
            if let Some(&c) = this.model.index_of.get(&name) {
                this.selected = Some(c);
                this.recompute_sections(c);
                this.all_types_open = true;
            }
        }
        this
    }

    /// What ⌘K focuses — the search input, so the recent-search list opens
    /// with it. Keys still reach `on_key_down` on the panel root, which is an
    /// ancestor of the input in the focus dispatch path.
    pub fn focus_handle(&self) -> FocusHandle {
        self.search_focus.clone()
    }

    /// Swap in a different slice of the schema (mode change).
    pub fn set_model(&mut self, model: Rc<Model>, cx: &mut Context<Self>) {
        self.all_sorted = sorted_cards(&model);
        // Keep the picked root. Picking one funnels back through here (
        // `RootPicked` → `Workspace::rebuild` → `set_model`), so recomputing
        // it would snap the highlight straight back to the default and leave
        // the button looking dead while the canvas showed the new root. Only
        // fall back when the name is gone — a different schema was loaded.
        if !declares_root(&model, self.root_pick.as_deref()) {
            self.root_pick = first_root(&model);
        }
        self.model = model;
        self.search.clear();
        self.kind_filter.clear();
        self.selected = None;
        self.pinned = None;
        self.implementers.clear();
        self.union_members.clear();
        self.referenced_by.clear();
        self.context_menu_field = None;
        self.refresh();
        cx.notify();
    }

    fn refresh(&mut self) {
        self.results = if self.search.text.trim().is_empty() {
            Vec::new()
        } else {
            search_graph(&self.model.graph, &self.search.text)
        };
        self.kind_counts.clear();
        for r in &self.results {
            *self.kind_counts.entry(r.type_kind).or_insert(0) += 1;
        }
        self.filtered = self
            .results
            .iter()
            .enumerate()
            .filter(|(_, r)| self.kind_filter.is_empty() || self.kind_filter.contains(&r.type_kind))
            .map(|(i, _)| i)
            .collect();
        self.active = 0;
        self.results_scroll.scroll_to_item(0);
    }

    fn set_query(&mut self, q: String, cx: &mut Context<Self>) {
        self.search.set_text(q);
        self.refresh();
        cx.notify();
    }

    fn toggle_kind(&mut self, kind: NodeKind, cx: &mut Context<Self>) {
        if !self.kind_filter.insert(kind) {
            self.kind_filter.remove(&kind);
        }
        self.refresh();
        cx.notify();
    }

    fn clear_kind_filter(&mut self, cx: &mut Context<Self>) {
        self.kind_filter.clear();
        self.refresh();
        cx.notify();
    }

    fn remove_history(&mut self, q: &str, cx: &mut Context<Self>) {
        self.search_history.retain(|s| s != q);
        crate::config::set_search_history(&self.search_history);
        cx.notify();
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        self.search_history.clear();
        crate::config::set_search_history(&self.search_history);
        cx.notify();
    }

    /// Recompute the three context sections for `card`.
    fn recompute_sections(&mut self, card: u32) {
        let model = self.model.clone();
        let name = model.cards[card as usize].name.clone();
        let kind = model.cards[card as usize].kind;
        let by_name = |a: &(u32, Vec<SharedString>), b: &(u32, Vec<SharedString>)| {
            model.cards[a.0 as usize].name.cmp(&model.cards[b.0 as usize].name)
        };

        let mut implementers: Vec<(u32, Vec<SharedString>)> = Vec::new();
        if kind == NodeKind::Interface {
            for c in &model.cards {
                if c.implements.contains(&name) {
                    let others = c.implements.iter().filter(|i| **i != name).cloned().collect();
                    implementers.push((c.index, others));
                }
            }
            implementers.sort_by(by_name);
        }

        let mut union_members: Vec<(u32, Vec<SharedString>)> = Vec::new();
        if kind == NodeKind::Union {
            for r in &model.cards[card as usize].rows {
                if r.kind != RowKind::UnionMember {
                    continue;
                }
                let Some(t) = r.target else { continue };
                let others = model.cards[t as usize]
                    .member_of_unions
                    .iter()
                    .filter(|u| **u != name)
                    .cloned()
                    .collect();
                union_members.push((t, others));
            }
            union_members.sort_by(by_name);
        }

        let mut referenced_by: Vec<(u32, Vec<SharedString>)> = Vec::new();
        for c in &model.cards {
            let fields: Vec<SharedString> = c
                .rows
                .iter()
                .filter(|r| r.kind == RowKind::Field && r.target == Some(card))
                .map(|r| SharedString::from(format!(".{}", r.left)))
                .collect();
            if !fields.is_empty() {
                referenced_by.push((c.index, fields));
            }
        }
        referenced_by.sort_by(by_name);

        self.implementers = implementers;
        self.union_members = union_members;
        self.referenced_by = referenced_by;
    }

    /// Focus `card` in the detail pane and tell the canvas to navigate.
    fn select_card(&mut self, card: u32, row: Option<usize>, cx: &mut Context<Self>) {
        self.selected = Some(card);
        self.pinned = row
            .and_then(|r| self.model.cards[card as usize].display_row(r))
            .map(|d| (card, d));
        self.recompute_sections(card);
        cx.emit(TreeEvent::Select { node_index: card as usize, row });
        cx.notify();
    }

    /// `"Type.field"` — the row's own name, not whatever it targets.
    fn copy_field_path(&mut self, card: u32, dix: usize, cx: &mut Context<Self>) {
        let c = &self.model.cards[card as usize];
        let path = format!("{}.{}", c.name, c.rows[dix].left);
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(path));
        self.context_menu_field = None;
        cx.notify();
    }

    /// Commit search result `ix` (an index into `filtered`) — the web's
    /// `jumpToAndClose`: remember the query, jump, then empty the box.
    fn select_result(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(&ri) = self.filtered.get(ix) else { return };
        let (card, row) = {
            let r = &self.results[ri];
            (r.node_index as u32, r.row_index)
        };
        if !self.search.text.trim().is_empty() {
            crate::config::push_search(&self.search.text);
            self.search_history = crate::config::search_history();
        }
        self.active = ix;
        self.select_card(card, row, cx);
        self.search.clear();
        self.refresh();
    }

    fn pick_root(&mut self, name: SharedString, cx: &mut Context<Self>) {
        self.root_pick = Some(name.clone());
        cx.emit(TreeEvent::RootPicked(name.to_string()));
        cx.notify();
    }

    fn is_navigable(&self, type_name: &str) -> bool {
        !BUILTIN.contains(&type_name) && self.model.index_of.contains_key(type_name)
    }

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let shift = ks.modifiers.shift;
        // ⌘-chords. ⌘←/⌘→ are line start/end, the way they are in every other
        // single-line field on this platform.
        if ks.modifiers.platform {
            match ks.key.as_str() {
                "a" => self.search.select_all(),
                "c" => {
                    if let Some(sel) = self.search.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(sel.to_string()));
                    }
                    return;
                }
                "x" => {
                    let Some(sel) = self.search.selected_text().map(str::to_string) else {
                        return;
                    };
                    cx.write_to_clipboard(ClipboardItem::new_string(sel));
                    self.search.delete_selection();
                    self.refresh();
                }
                "v" => {
                    let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
                        return;
                    };
                    self.search.insert(&text);
                    self.refresh();
                }
                "left" => self.search.move_cursor(0, shift),
                "right" => self.search.move_cursor(self.search.text.len(), shift),
                _ => return,
            }
            cx.notify();
            return;
        }
        if ks.modifiers.control {
            return;
        }
        match ks.key.as_str() {
            "backspace" => {
                self.search.backspace();
                self.refresh();
            }
            "delete" => {
                self.search.delete_forward();
                self.refresh();
            }
            "left" => {
                let to = self.search.prev_boundary(self.search.cursor);
                self.search.move_cursor(to, shift);
            }
            "right" => {
                let to = self.search.next_boundary(self.search.cursor);
                self.search.move_cursor(to, shift);
            }
            "home" => self.search.move_cursor(0, shift),
            "end" => self.search.move_cursor(self.search.text.len(), shift),
            "escape" => {
                self.search.clear();
                self.refresh();
                window.blur();
            }
            // Up/down/enter drive the result list, not the text — there is
            // only ever one line to move within.
            "up" => {
                self.active = self.active.saturating_sub(1);
                self.results_scroll.scroll_to_item(self.active);
            }
            "down" => {
                self.active = (self.active + 1).min(self.filtered.len().saturating_sub(1));
                self.results_scroll.scroll_to_item(self.active);
            }
            "enter" => {
                self.select_result(self.active, cx);
                return;
            }
            _ => {
                if let Some(ch) = ks.key_char.as_deref() {
                    if !ch.chars().any(|c| c.is_control()) {
                        self.search.insert(ch);
                        self.refresh();
                    }
                } else {
                    return;
                }
            }
        }
        cx.notify();
    }

    // ---- 1. search input -------------------------------------------------

    fn render_search(&self, th: Theme, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search.text.clone();
        let empty = query.is_empty();
        let text: SharedString = if empty {
            "Search types & fields…".into()
        } else {
            query.clone().into()
        };
        // The caret and the selection band are placed by measuring the text
        // to their left, which is exact because the box is monospaced.
        let caret_x = mono_w(&query[..self.search.cursor], SEARCH_FONT_PX);
        let selection = self.search.selection().map(|(s, e)| {
            (mono_w(&query[..s], SEARCH_FONT_PX), mono_w(&query[s..e], SEARCH_FONT_PX))
        });
        let origin = self.search_origin.clone();
        div()
            .flex_none()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(th.panel_border)
            .child(
                div()
                    .w_full()
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px(px(8.0))
                    .rounded(px(4.0))
                    .border_1()
                    // The input owns the focus that drives the recent-search
                    // list. `track_focus` auto-focuses on mouse down, so
                    // hanging that off the panel root instead would make
                    // *any* sidebar click open the list.
                    .track_focus(&self.search_focus)
                    .border_color(if focused { th.accent } else { th.panel_border })
                    .bg(th.bg)
                    .child(icon(Icon::Search, px(12.0), th.text_muted))
                    .child(
                        div()
                            .id("search-text")
                            .flex_1()
                            .min_w_0()
                            .relative()
                            .h(px(SEARCH_LINE_H))
                            .flex()
                            .items_center()
                            .font_family(MONO)
                            .text_size(px(SEARCH_FONT_PX))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_color(if empty { th.text_muted } else { th.text })
                            // Records where the text starts so a click can be
                            // turned back into a caret offset.
                            .child(
                                gpui::canvas(
                                    |_, _, _| (),
                                    move |bounds, _, _, _| {
                                        origin.set(f32::from(bounds.origin.x))
                                    },
                                )
                                .absolute()
                                .size_full(),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, ev: &gpui::MouseDownEvent, _, cx| {
                                    let x = f32::from(ev.position.x) - this.search_origin.get();
                                    let to = offset_for_x(&this.search.text, x);
                                    this.search.move_cursor(to, false);
                                    cx.notify();
                                }),
                            )
                            // Painted before the glyphs so it sits behind them.
                            .when_some(selection, |el, (x, w)| {
                                el.child(
                                    div()
                                        .absolute()
                                        .left(px(x))
                                        .top_0()
                                        .bottom_0()
                                        .w(px(w))
                                        .rounded(px(2.0))
                                        .bg(th.accent.opacity(0.35)),
                                )
                            })
                            .child(text)
                            // No caret while a selection is up, matching the
                            // platform's own fields.
                            .when(focused && selection.is_none(), |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .left(px(caret_x))
                                        .top(px(2.0))
                                        .bottom(px(2.0))
                                        .w(px(1.5))
                                        .bg(th.accent),
                                )
                            }),
                    )
                    .child(if empty {
                        div()
                            .flex_none()
                            .font_family(MONO)
                            .text_size(px(10.0))
                            .text_color(th.text_muted.opacity(0.5))
                            .child("⌘K")
                            .into_any_element()
                    } else {
                        div()
                            .id("search-clear")
                            .flex_none()
                            .cursor_pointer()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.set_query(String::new(), cx)),
                            )
                            .child(icon(Icon::X, px(12.0), th.text_muted))
                            .into_any_element()
                    }),
            )
            .into_any_element()
    }

    // ---- 2. search history -----------------------------------------------

    fn render_history(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.search_history.clone().into_iter().enumerate().map(|(i, q)| {
            let fill = q.clone();
            let del = q.clone();
            div()
                .id(("history", i))
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py(px(6.0))
                .cursor_pointer()
                .hover(|el| el.bg(th.hover_bg))
                .on_click(cx.listener(move |this, _, _, cx| this.set_query(fill.clone(), cx)))
                .child(icon(Icon::Clock, px(12.0), th.text_muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(MONO)
                        .text_size(px(12.0))
                        .text_color(th.text)
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(SharedString::from(q)),
                )
                .child(
                    div()
                        .id(("history-del", i))
                        .flex_none()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.remove_history(&del, cx);
                        }))
                        .child(icon(Icon::X, px(12.0), th.text_muted)),
                )
        });

        div()
            .id("history-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py(px(6.0))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(th.text_muted)
                            .child("RECENT"),
                    )
                    .child(
                        div()
                            .id("history-clear-all")
                            .cursor_pointer()
                            .text_size(px(10.0))
                            .text_color(th.text_muted)
                            .hover(|el| el.text_color(th.text))
                            .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx)))
                            .child("Clear all"),
                    ),
            )
            .children(rows)
            .into_any_element()
    }

    // ---- 3. kind filter chips --------------------------------------------

    fn render_kind_chips(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let chips = KIND_ORDER
            .into_iter()
            .filter(|(k, _)| self.kind_counts.get(k).copied().unwrap_or(0) > 0)
            .map(|(kind, label)| {
                let active = self.kind_filter.contains(&kind);
                let tone = th.kind_color(kind);
                let count = self.kind_counts.get(&kind).copied().unwrap_or(0);
                div()
                    .id(label)
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_full()
                    .border_1()
                    .px_2()
                    .py(px(2.0))
                    .text_size(px(10.0))
                    .cursor_pointer()
                    .border_color(if active { transparent_black() } else { th.card_border })
                    .when(active, |el| el.bg(tone.opacity(0.1)).text_color(tone))
                    .when(!active, |el| el.text_color(th.text_muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_kind(kind, cx)))
                    .child(SharedString::from(label))
                    .child(
                        div()
                            .font_family(MONO)
                            .text_size(px(9.0))
                            .text_color(if active { tone } else { th.text_muted.opacity(0.7) })
                            .child(SharedString::from(count.to_string())),
                    )
            });

        div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .px_3()
            .py(px(6.0))
            .border_b_1()
            .border_color(th.panel_border)
            .children(chips)
            .when(!self.kind_filter.is_empty(), |el| {
                el.child(
                    div()
                        .id("kind-clear")
                        .ml_auto()
                        .flex()
                        .items_center()
                        .gap_1()
                        .rounded_full()
                        .px(px(6.0))
                        .py(px(2.0))
                        .text_size(px(10.0))
                        .text_color(th.text_muted)
                        .cursor_pointer()
                        .hover(|el| el.text_color(th.text))
                        .on_click(cx.listener(|this, _, _, cx| this.clear_kind_filter(cx)))
                        .child(icon(Icon::X, px(10.0), th.text_muted))
                        .child("Clear"),
                )
            })
            .into_any_element()
    }

    // ---- 4. results list --------------------------------------------------

    fn render_results(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let rows = self
            .filtered
            .iter()
            .copied()
            .enumerate()
            .map(|(i, ri)| self.render_result_row(i, ri, th, cx))
            .collect::<Vec<_>>();
        let empty = rows.is_empty();

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .when(!self.results.is_empty(), |el| {
                el.child(self.render_kind_chips(th, cx))
            })
            .child(
                div()
                    .id("results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.results_scroll)
                    .when(empty, |el| {
                        el.child(
                            div()
                                .p(px(24.0))
                                .text_center()
                                .text_size(px(12.0))
                                .text_color(th.text_muted)
                                .child("No results"),
                        )
                    })
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_result_row(
        &self,
        ix: usize,
        ri: usize,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let r = &self.results[ri];
        let selected = ix == self.active;

        // Line 1: kind badge · name · raw return type.
        let name_block = match &r.field_name {
            Some(field) => div()
                .flex()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .text_color(th.text_muted)
                        .child(highlighted(
                            &r.type_name,
                            r.type_match_indices.as_deref().unwrap_or(&[]),
                            th.primary,
                        )),
                )
                .child(div().flex_none().text_color(th.text_muted).child("."))
                .child(
                    div()
                        .flex_none()
                        .text_color(th.text)
                        .child(highlighted(field, &r.match_indices, th.primary)),
                ),
            None => div()
                .flex()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .text_color(th.text)
                        .child(highlighted(&r.type_name, &r.match_indices, th.primary)),
                ),
        };

        let line1 = div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .child(kind_badge(th, r.type_kind, kind_label(r.type_kind)))
            .child(name_block)
            .when_some(r.field_type.clone(), |el, ft| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(th.text_muted)
                        .child(SharedString::from(ft)),
                )
            });

        // Line 2: the prose snippet, tagged by which prose field matched.
        let line2 = r.snippet.as_ref().map(|s| {
            let deprecated = r.snippet_kind == Some(SnippetKind::DeprecationReason);
            let (tag_bg, tag_fg, tag) = if deprecated {
                (th.type_amber.opacity(0.15), th.type_amber, "deprecated")
            } else {
                (th.active_bg, th.text_muted, "desc")
            };
            div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1()
                .pl(px(4.0))
                .text_size(px(10.0))
                .text_color(th.text_muted)
                .child(
                    div()
                        .flex_none()
                        .rounded(px(4.0))
                        .px(px(4.0))
                        .py(px(1.0))
                        .text_size(px(9.0))
                        .bg(tag_bg)
                        .text_color(tag_fg)
                        .child(tag),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .italic()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(highlighted(&s.snippet, &s.indices, th.primary)),
                )
        });

        div()
            .id(("result", ix))
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px_3()
            .py(px(6.0))
            .font_family(MONO)
            .text_size(px(12.0))
            .cursor_pointer()
            .when(selected, |el| el.bg(th.active_bg))
            .hover(|el| el.bg(th.hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| this.select_result(ix, cx)))
            .child(line1)
            .children(line2)
            .into_any_element()
    }

    // ---- 5. root type selector -------------------------------------------

    fn render_roots(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let label: SharedString = if self.model.schema_name.is_empty() {
            "Schema".into()
        } else {
            self.model.schema_name.clone().into()
        };
        let buttons = root_names(&self.model).into_iter().enumerate().map(|(i, name)| {
            let active = self.root_pick.as_ref() == Some(&name);
            let pick = name.clone();
            div()
                .id(("root", i))
                .rounded(px(4.0))
                .px_2()
                .py_1()
                .font_family(MONO)
                .text_size(px(12.0))
                .cursor_pointer()
                .when(active, |el| el.bg(th.primary).text_color(th.primary_fg))
                .when(!active, |el| el.bg(th.active_bg).text_color(th.text_muted))
                .on_click(cx.listener(move |this, _, _, cx| this.pick_root(pick.clone(), cx)))
                .child(name)
        });

        div()
            .flex_none()
            .p_3()
            .border_b_1()
            .border_color(th.panel_border)
            .child(
                div()
                    .mb(px(4.0))
                    .text_size(px(10.0))
                    .text_color(th.text_muted)
                    .child(label),
            )
            .child(div().flex().flex_wrap().gap_1().children(buttons))
            .into_any_element()
    }

    // ---- 6. "All types" ---------------------------------------------------

    fn render_all_types(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.all_sorted.is_empty() {
            return div().into_any_element();
        }
        let open = self.all_types_open;
        let count = self.all_sorted.len();
        let mut section = div()
            .flex_none()
            .border_b_1()
            .border_color(th.panel_border)
            .child(
                div()
                    .id("all-types-toggle")
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .text_size(px(10.0))
                    .text_color(th.text_muted)
                    .cursor_pointer()
                    .hover(|el| el.bg(th.hover_bg))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.all_types_open = !this.all_types_open;
                        cx.notify();
                    }))
                    .child(icon(
                        if open { Icon::ChevronDown } else { Icon::ChevronRight },
                        px(12.0),
                        th.text_muted,
                    ))
                    .child(SharedString::from(format!("All types ({count})"))),
            );

        if open {
            section = section.child(
                uniform_list(
                    "all-types",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                        let th = crate::theme::current(cx, window.appearance());
                        range
                            .map(|ix| {
                                let card = this.all_sorted[ix];
                                this.type_row("all-type", ix, card, &[], th, cx)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .h(px((count as f32 * LIST_ROW_H).min(LIST_MAX_H)))
                .border_t_1()
                .border_color(th.panel_border),
            );
        }
        section.into_any_element()
    }

    // ---- 7. context sections ----------------------------------------------

    fn render_sections(&self, th: Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if !self.implementers.is_empty() {
            out.push(self.render_section(
                "implemented-by",
                format!("Implemented by ({})", self.implementers.len()),
                self.implementers.len(),
                th,
                cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                    let th = crate::theme::current(cx, window.appearance());
                    range
                        .map(|ix| {
                            let (card, chips) = this.implementers[ix].clone();
                            this.type_row("impl-row", ix, card, &chips, th, cx)
                        })
                        .collect::<Vec<_>>()
                }),
            ));
        }
        if !self.union_members.is_empty() {
            out.push(self.render_section(
                "members",
                format!("Members ({})", self.union_members.len()),
                self.union_members.len(),
                th,
                cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                    let th = crate::theme::current(cx, window.appearance());
                    range
                        .map(|ix| {
                            let (card, chips) = this.union_members[ix].clone();
                            this.type_row("member-row", ix, card, &chips, th, cx)
                        })
                        .collect::<Vec<_>>()
                }),
            ));
        }
        if !self.referenced_by.is_empty() {
            out.push(self.render_section(
                "referenced-by",
                format!("Referenced by ({})", self.referenced_by.len()),
                self.referenced_by.len(),
                th,
                cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                    let th = crate::theme::current(cx, window.appearance());
                    range
                        .map(|ix| {
                            let (card, chips) = this.referenced_by[ix].clone();
                            this.type_row("ref-row", ix, card, &chips, th, cx)
                        })
                        .collect::<Vec<_>>()
                }),
            ));
        }
        out
    }

    fn render_section<R: IntoElement>(
        &self,
        id: &'static str,
        title: String,
        count: usize,
        th: Theme,
        items: impl 'static + Fn(std::ops::Range<usize>, &mut Window, &mut App) -> Vec<R>,
    ) -> AnyElement {
        div()
            .flex_none()
            .border_b_1()
            .border_color(th.panel_border)
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .text_size(px(10.0))
                    .text_color(th.text_muted)
                    .child(SharedString::from(title)),
            )
            .child(
                uniform_list(id, count, items)
                    .h(px((count as f32 * LIST_ROW_H).min(LIST_MAX_H)))
                    .border_t_1()
                    .border_color(th.panel_border),
            )
            .into_any_element()
    }

    /// A capped-list row: kind badge, truncated name, trailing chips.
    fn type_row(
        &self,
        ns: &'static str,
        ix: usize,
        card: u32,
        chips: &[SharedString],
        th: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = &self.model.cards[card as usize];
        let selected = self.selected == Some(card);
        let badge = kind_badge(th, c.kind, c.kind_label);
        let badge = if selected {
            badge.bg(th.primary_fg.opacity(0.2)).text_color(th.primary_fg)
        } else {
            badge
        };
        let chip_els = chips
            .iter()
            .cloned()
            .map(|label| {
                div()
                    .flex_none()
                    .rounded(px(4.0))
                    .px(px(4.0))
                    .py(px(1.0))
                    .text_size(px(9.0))
                    .bg(if selected { th.primary_fg.opacity(0.2) } else { th.active_bg })
                    .text_color(if selected { th.primary_fg } else { th.text_muted })
                    .child(label)
            })
            .collect::<Vec<_>>();
        let has_chips = !chip_els.is_empty();

        div()
            .id((ns, ix))
            .w_full()
            .h(px(LIST_ROW_H))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .font_family(MONO)
            .text_size(px(12.0))
            .cursor_pointer()
            .when(selected, |el| el.bg(th.primary).text_color(th.primary_fg))
            .when(!selected, |el| {
                el.text_color(th.text).hover(|el| el.bg(th.hover_bg))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select_card(card, None, cx)))
            .child(badge)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(c.name.clone()),
            )
            .when(has_chips, |el| {
                el.child(
                    div()
                        .ml_auto()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1()
                        .children(chip_els),
                )
            })
            .into_any_element()
    }

    // ---- 8. TypeDetail -----------------------------------------------------

    fn render_detail(&self, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        let pane = div().id("type-detail").flex_1().min_h_0().overflow_y_scroll();
        let Some(card) = self.selected else {
            return pane
                .child(
                    div()
                        .p(px(24.0))
                        .text_center()
                        .text_size(px(12.0))
                        .text_color(th.text_muted)
                        .child("Select a type to start exploring."),
                )
                .into_any_element();
        };
        let c = &self.model.cards[card as usize];

        // The implements band becomes clickable interface buttons.
        let iface_row = (!c.implements.is_empty()).then(|| {
            let buttons = c.implements.iter().cloned().enumerate().map(|(i, name)| {
                let navigable = self.is_navigable(&name);
                let target = self.model.index_of.get(name.as_ref()).copied();
                div()
                    .id(("iface", i))
                    .rounded(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .font_family(MONO)
                    .when(navigable, |el| {
                        el.cursor_pointer().bg(th.active_bg).hover(|el| el.bg(th.hover_bg))
                    })
                    .when(!navigable, |el| el.opacity(0.6))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(t) = target {
                            this.select_card(t, None, cx);
                        }
                    }))
                    .child(name)
            });
            div()
                .mb_3()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .text_size(px(11.0))
                .text_color(th.text_muted)
                .child("implements")
                .children(buttons)
        });

        // Body: one row per field / enum value / union member.
        let body: Vec<AnyElement> = if c.rows.is_empty() {
            let empty = if c.kind == NodeKind::Enum { "no values" } else { "no fields" };
            vec![div()
                .px_2()
                .py_1()
                .italic()
                .text_color(th.text_muted)
                .child(empty)
                .into_any_element()]
        } else {
            (0..c.rows.len())
                .map(|i| self.render_field_row(card, i, th, cx))
                .collect()
        };

        pane.child(
            div()
                .p_3()
                .child(
                    div()
                        .mb_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            kind_badge(th, c.kind, c.kind_label)
                                .px(px(8.0))
                                .py(px(2.0))
                                .text_size(px(12.0)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .font_family(MONO)
                                .text_size(px(14.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(th.text)
                                .child(c.name.clone()),
                        ),
                )
                .when_some(c.description.clone(), |el, d| {
                    el.child(
                        div()
                            .mb_3()
                            .text_size(px(12.0))
                            .text_color(th.text_muted)
                            .child(SharedString::from(d)),
                    )
                })
                .children(iface_row)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .font_family(MONO)
                        .text_size(px(12.0))
                        .children(body),
                ),
        )
        .into_any_element()
    }

    fn render_field_row(
        &self,
        card: u32,
        dix: usize,
        th: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = &self.model.cards[card as usize];
        let row = &c.rows[dix];
        let expired = row.until_expired;
        let deprecated = row.deprecated;
        let pinned = self.pinned == Some((card, dix));
        let is_member = row.kind == RowKind::UnionMember;
        let target = row.target;
        let navigable = target.is_some();
        // Graph row index — the canvas maps it back through `row_map`.
        let graph_row = c.row_map.iter().position(|&m| m == Some(dix as u32));

        let label: SharedString = if is_member {
            format!("| {}", row.left).into()
        } else {
            row.left.clone()
        };

        // Line 1, left: field name (+ arity badge).
        let left = div()
            .flex()
            .min_w_0()
            .items_center()
            .gap_1()
            .child(
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(if expired { th.red } else { th.text })
                    .when(deprecated, |el| el.line_through())
                    .child(label),
            )
            .when(!row.args.is_empty(), |el| {
                el.child(
                    div()
                        .flex_none()
                        .text_size(px(10.0))
                        .text_color(th.text_muted)
                        .child(SharedString::from(format!(
                            "({}/{})",
                            row.required_args,
                            row.args.len()
                        ))),
                )
            });

        // Line 1, right: the return type chip.
        let mut chip = div()
            .id(("field-type", dix))
            .flex()
            .min_w_0()
            .items_center()
            .gap(px(2.0))
            .rounded(px(4.0))
            .when(row.is_relay, |el| {
                el.child(icon(Icon::Link2, px(10.0), th.relay_orange))
            });
        if !row.right.is_empty() {
            chip = chip.child(
                div()
                    .min_w_0()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(th.type_amber)
                    .child(row.right.clone()),
            );
        }
        if navigable {
            chip = chip
                .cursor_pointer()
                .hover(|el| el.bg(th.primary.opacity(0.15)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some(t) = target {
                        this.select_card(t, None, cx);
                    }
                }))
                .child(icon(Icon::ChevronRight, px(12.0), th.text_muted));
        }

        let line1 = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(left)
            .child(div().flex().min_w_0().items_center().justify_end().child(chip));

        // Line 2: the deprecation note. Line 3: the description.
        let note = deprecated.then(|| {
            let text: SharedString = row
                .deprecation_reason
                .clone()
                .map(SharedString::from)
                .unwrap_or_else(|| "Deprecated".into());
            let tone = if expired { th.red } else { th.type_amber };
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_size(px(11.0))
                .text_color(tone)
                .child(icon(Icon::TriangleAlert, px(10.0), tone))
                .child(text)
        });
        let desc = row.description.clone().map(|d| {
            div()
                .text_size(px(11.0))
                .text_color(th.text_muted)
                .child(SharedString::from(d))
        });

        let field_path = format!("{}.{}", c.name, row.left);
        let menu_open = self.context_menu_field == Some((card, dix));
        let field_menu = menu_open.then(|| {
            div()
                .id(("field-ctx-menu", dix))
                .absolute()
                .top(px(28.0))
                .right(px(4.0))
                .min_w(px(180.0))
                .rounded_lg()
                .border_1()
                .border_color(th.card_border)
                .bg(th.chrome_bg)
                .shadow_lg()
                .py_1()
                .font_family("Menlo")
                .text_size(px(12.0))
                .text_color(th.text)
                // See the same trick in canvas.rs's node context menu: without
                // this a click on the item also bubbles down to the row's own
                // `on_click` underneath and navigates instead of copying.
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("ctx-copy-field-path")
                        .px_3()
                        .py(px(6.0))
                        .cursor_pointer()
                        .hover(|el| el.bg(th.hover_bg))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy_field_path(card, dix, cx);
                        }))
                        .child(SharedString::from(format!("Copy \"{field_path}\""))),
                )
        });

        div()
            .id(("field", dix))
            .relative()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .rounded(px(4.0))
            .px_2()
            .py_1()
            .border_1()
            .border_color(if pinned { th.pin } else { transparent_black() })
            .cursor_pointer()
            .when(pinned, |el| el.bg(th.pin.opacity(0.1)))
            .when(!pinned && expired, |el| el.bg(th.red.opacity(0.1)))
            .when(!pinned && !expired && deprecated, |el| {
                el.bg(th.type_amber.opacity(0.1)).opacity(0.6)
            })
            .hover(|el| el.bg(th.hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| {
                if is_member {
                    if let Some(t) = target {
                        this.select_card(t, None, cx);
                    }
                } else {
                    this.select_card(card, graph_row, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.context_menu_field = Some((card, dix));
                    cx.notify();
                }),
            )
            .child(line1)
            .children(note)
            .children(desc)
            .children(field_menu)
            .into_any_element()
    }
}

/// Merge sorted char indices into byte ranges over `text`.
fn char_byte_ranges(text: &str, char_idxs: &[usize]) -> Vec<std::ops::Range<usize>> {
    let set: std::collections::BTreeSet<usize> = char_idxs.iter().copied().collect();
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for (ci, (bi, c)) in text.char_indices().enumerate() {
        if set.contains(&ci) {
            let end = bi + c.len_utf8();
            match out.last_mut() {
                Some(last) if last.end == bi => last.end = end,
                _ => out.push(bi..end),
            }
        }
    }
    out
}

/// Fuzzy-match highlight: matched chars bold in `color`, no background.
fn highlighted(text: &str, char_idxs: &[usize], color: Hsla) -> AnyElement {
    if char_idxs.is_empty() {
        return SharedString::from(text.to_owned()).into_any_element();
    }
    let ranges = char_byte_ranges(text, char_idxs);
    let style = HighlightStyle {
        color: Some(color),
        font_weight: Some(FontWeight::BOLD),
        ..Default::default()
    };
    StyledText::new(text.to_owned())
        .with_highlights(ranges.into_iter().map(|r| (r, style)))
        .into_any_element()
}

fn sorted_cards(model: &Model) -> Vec<u32> {
    let mut all: Vec<u32> = (0..model.cards.len() as u32).collect();
    all.sort_by(|&a, &b| model.cards[a as usize].name.cmp(&model.cards[b as usize].name));
    all
}

/// Byte offset whose caret position sits closest to `x`, measured in pixels
/// from the start of the text. Only char boundaries are candidates, so the
/// caret can never land inside a multi-byte glyph.
fn offset_for_x(text: &str, x: f32) -> usize {
    text.char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .min_by(|&a, &b| {
            let da = (mono_w(&text[..a], SEARCH_FONT_PX) - x).abs();
            let db = (mono_w(&text[..b], SEARCH_FONT_PX) - x).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(0)
}

/// Every root operation the schema *declares*, in Query → Mutation →
/// Subscription order.
///
/// Read from `root_types` rather than the model's cards on purpose: the
/// Reachable slice only ever keeps one root's subgraph, so the other roots
/// have no card to find and would drop out of the picker entirely.
fn root_names(model: &Model) -> Vec<SharedString> {
    let rt = &model.graph.root_types;
    [&rt.query, &rt.mutation, &rt.subscription]
        .into_iter()
        .filter_map(|n| n.clone().map(SharedString::from))
        .collect()
}

fn first_root(model: &Model) -> Option<SharedString> {
    root_names(model).into_iter().next()
}

/// Whether `name` is still one of this schema's declared roots.
fn declares_root(model: &Model, name: Option<&str>) -> bool {
    name.is_some_and(|name| root_names(model).iter().any(|r| r.as_ref() == name))
}

impl EventEmitter<TreeEvent> for TreePanel {}

impl Focusable for TreePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TreePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        let focused = self.search_focus.is_focused(window);
        let query_empty = self.search.text.trim().is_empty();
        // The web hides the tree while the recent list is showing. Keyed on
        // the *input's* focus: keyed on the panel's, every click in the
        // sidebar would swap the tree out from under the cursor and the
        // press would never land on the row it was aimed at.
        let show_history = focused && query_empty && !self.search_history.is_empty();

        let mut root = div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(th.panel)
            .border_r_1()
            .border_color(th.panel_border)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.context_menu_field.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(self.render_search(th, focused, cx));

        if show_history {
            root = root.child(self.render_history(th, cx));
        } else if !query_empty {
            root = root.child(self.render_results(th, cx));
        } else {
            root = root
                .child(self.render_roots(th, cx))
                .child(self.render_all_types(th, cx))
                .children(self.render_sections(th, cx))
                .child(self.render_detail(th, cx));
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{build_model, slice_graph, Mode, ModelOptions};
    use graviz_core::graph::{sdl_to_graph, SdlToGraphOptions};

    const THREE_ROOTS: &str = "type Query { post: Post }
         type Mutation { createPost: Post }
         type Subscription { postCreated: Post }
         type Post { id: ID! }";

    fn model_of(sdl: &str, root: Option<&str>) -> Model {
        let g = sdl_to_graph(
            sdl,
            &SdlToGraphOptions { hide_relay_boilerplate: false, ..Default::default() },
        );
        assert!(g.error.is_none(), "{:?}", g.error);
        build_model(
            slice_graph(&g, Mode::Reachable, root),
            "t".into(),
            &ModelOptions { today: "2020-01-01".into(), ..Default::default() },
        )
    }

    /// The picker must list every root the schema declares. The Reachable
    /// slice only keeps one root's subgraph, so deriving the list from the
    /// model's own cards showed just that one.
    #[test]
    fn root_names_survive_a_single_root_slice() {
        let m = model_of(THREE_ROOTS, Some("Mutation"));
        assert_eq!(m.roots.len(), 1, "the slice really does keep only one root card");
        let names: Vec<String> = root_names(&m).iter().map(|n| n.to_string()).collect();
        assert_eq!(names, ["Query", "Mutation", "Subscription"]);
    }

    /// Picking a root re-slices and lands back in `set_model`, which must
    /// keep the pick — recomputing it snapped the highlight back to Query
    /// while the canvas showed the newly picked root.
    #[test]
    fn a_picked_root_is_still_recognized_after_reslicing() {
        let m = model_of(THREE_ROOTS, Some("Mutation"));
        assert!(declares_root(&m, Some("Mutation")));
        assert!(declares_root(&m, Some("Subscription")));
        assert!(!declares_root(&m, Some("Post")), "not a root operation");
        assert!(!declares_root(&m, None));
        assert_eq!(first_root(&m).map(|s| s.to_string()), Some("Query".into()));
    }

    /// Clicking maps an x offset back to a caret position, snapping to the
    /// nearer boundary so the caret lands where the pointer looks.
    #[test]
    fn click_maps_x_to_the_nearest_caret_offset() {
        let w = |s: &str| mono_w(s, SEARCH_FONT_PX);
        assert_eq!(offset_for_x("user", 0.0), 0);
        assert_eq!(offset_for_x("user", w("user")), 4, "past the end clamps to the end");
        assert_eq!(offset_for_x("user", w("user") + 999.0), 4);
        assert_eq!(offset_for_x("user", w("us")), 2);
        // Just past a glyph's midpoint rounds on to the next boundary.
        assert_eq!(offset_for_x("user", w("us") + w("e") * 0.6), 3);
        assert_eq!(offset_for_x("", 42.0), 0, "empty text has only offset 0");
    }

    /// Byte offsets again: a click inside a multi-byte glyph has to resolve
    /// to one of its edges, never into the middle.
    #[test]
    fn click_never_lands_inside_a_multibyte_glyph() {
        let text = "한글";
        for step in 0..40 {
            let off = offset_for_x(text, step as f32 * 2.0);
            assert!(text.is_char_boundary(off), "offset {off} splits a glyph");
        }
    }

    /// A schema whose roots are gone (a different file was opened) has to
    /// fall back rather than keep highlighting a name that is not there.
    #[test]
    fn declares_root_rejects_names_from_another_schema() {
        let m = model_of("type Query { a: String }", None);
        assert!(declares_root(&m, Some("Query")));
        assert!(!declares_root(&m, Some("Mutation")));
    }
}
