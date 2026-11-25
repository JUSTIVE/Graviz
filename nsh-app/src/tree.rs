//! The Reachable tab: the search box (with its history and kind chips),
//! the root operations, all types, and the selected type's detail with what
//! implements, contains and references it.

use std::collections::HashSet;

use graviz_core::{
    graph::NodeKind,
    search::{SearchResult, SnippetKind, search_graph},
};
use nsh_core::prelude::*;
use nsh_metro::{Icon, TextStyle, theme::SEMIBOLD};

use crate::{
    config,
    model::{Model, RowKind},
    store::{Shared, Store},
    theme::Color,
    ui::{Glyph, MONO, c, glyph, icon, kind_badge, kind_label, metro, mono, th},
};

type S = crate::model::SharedString;

const KIND_ORDER: [(NodeKind, &str); 6] = [
    (NodeKind::Object, "type"),
    (NodeKind::Interface, "interface"),
    (NodeKind::Union, "union"),
    (NodeKind::Enum, "enum"),
    (NodeKind::Input, "input"),
    (NodeKind::Scalar, "scalar"),
];
const BUILTIN: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];
const LIST_ROW_H: f32 = 24.0;
const LIST_MAX_H: f32 = 192.0;

/// The tree's own state.
#[derive(Clone, Copy)]
struct Tree {
    store: Store,
    query: Signal<String>,
    kind_filter: Signal<Vec<NodeKind>>,
    active: Signal<usize>,
    history: Signal<Vec<String>>,
    selected: Signal<Option<u32>>,
    pinned: Signal<Option<(u32, usize)>>,
    all_types_open: Signal<bool>,
    root_pick: Signal<Option<String>>,
    search_focused: Signal<bool>,
    menu_field: Signal<Option<(u32, usize)>>,
    search_ref: ElementRef,
}

fn root_names(model: &Model) -> Vec<String> {
    let rt = &model.graph.root_types;
    [&rt.query, &rt.mutation, &rt.subscription].into_iter().filter_map(|n| n.clone()).collect()
}

fn sorted_cards(model: &Model) -> Vec<u32> {
    let mut all: Vec<u32> = (0..model.cards.len() as u32).collect();
    all.sort_by(|&a, &b| model.cards[a as usize].name.cmp(&model.cards[b as usize].name));
    all
}

/// Hits for the query, and the ones the kind chips let through.
fn results(model: &Model, query: &str, filter: &[NodeKind]) -> (Vec<SearchResult>, Vec<usize>) {
    let results = if query.trim().is_empty() { Vec::new() } else { search_graph(&model.graph, query) };
    let filtered =
        results.iter().enumerate().filter(|(_, r)| filter.is_empty() || filter.contains(&r.type_kind)).map(|(i, _)| i).collect();
    (results, filtered)
}

/// `(card, extra chips)` for each of the three context sections.
struct Sections {
    implementers: Vec<(u32, Vec<S>)>,
    union_members: Vec<(u32, Vec<S>)>,
    referenced_by: Vec<(u32, Vec<S>)>,
}

fn sections(model: &Model, card: u32) -> Sections {
    let name = model.cards[card as usize].name.clone();
    let kind = model.cards[card as usize].kind;
    let by_name = |a: &(u32, Vec<S>), b: &(u32, Vec<S>)| model.cards[a.0 as usize].name.cmp(&model.cards[b.0 as usize].name);
    let mut implementers = Vec::new();
    if kind == NodeKind::Interface {
        for c in &model.cards {
            if c.implements.contains(&name) {
                implementers.push((c.index, c.implements.iter().filter(|i| **i != name).cloned().collect()));
            }
        }
        implementers.sort_by(by_name);
    }
    let mut union_members = Vec::new();
    if kind == NodeKind::Union {
        for r in &model.cards[card as usize].rows {
            if r.kind != RowKind::UnionMember {
                continue;
            }
            let Some(t) = r.target else { continue };
            let others = model.cards[t as usize].member_of_unions.iter().filter(|u| **u != name).cloned().collect();
            union_members.push((t, others));
        }
        union_members.sort_by(by_name);
    }
    let mut referenced_by = Vec::new();
    for c in &model.cards {
        let fields: Vec<S> = c
            .rows
            .iter()
            .filter(|r| r.kind == RowKind::Field && r.target == Some(card))
            .map(|r| S::from(format!(".{}", r.left)))
            .collect();
        if !fields.is_empty() {
            referenced_by.push((c.index, fields));
        }
    }
    referenced_by.sort_by(by_name);
    Sections { implementers, union_members, referenced_by }
}

/// `text` with the characters at `idxs` bold, in `color`.
pub fn highlighted(text_: &str, idxs: &[usize], color: impl Fn() -> Hsla + Copy + 'static) -> Text {
    if idxs.is_empty() {
        return text(text_.to_string());
    }
    let set: HashSet<usize> = idxs.iter().copied().collect();
    let mut runs: Vec<(String, bool)> = Vec::new();
    for (i, ch) in text_.chars().enumerate() {
        let hit = set.contains(&i);
        match runs.last_mut() {
            Some((s, h)) if *h == hit => s.push(ch),
            _ => runs.push((ch.to_string(), hit)),
        }
    }
    let mut t = text("");
    for (s, hit) in runs {
        t = t.child(if hit { text(s).font_weight(FontWeight::BOLD).text_color(color) } else { text(s) });
    }
    t
}

impl Tree {
    fn model(&self) -> Option<Shared<Model>> {
        self.store.model.get()
    }

    fn model_untracked(&self) -> Option<Shared<Model>> {
        self.store.model.get_untracked()
    }

    /// Selects `card` in the detail pane and sends the canvas to it.
    fn select_card(&self, card: u32, row: Option<usize>) {
        let Some(m) = self.model_untracked() else { return };
        self.selected.set(Some(card));
        self.pinned.set(row.and_then(|r| m.cards[card as usize].display_row(r)).map(|d| (card, d)));
        self.store.navigate_to(card, row);
    }

    /// Commits filtered result `ix`: remember the query, jump, then empty
    /// the box (the web's `jumpToAndClose`).
    fn select_result(&self, ix: usize) {
        let Some(m) = self.model_untracked() else { return };
        let q = self.query.get_untracked();
        let (res, filtered) = results(&m, &q, &self.kind_filter.get_untracked());
        let Some(&ri) = filtered.get(ix) else { return };
        let r = &res[ri];
        if !q.trim().is_empty() {
            config::push_search(&q);
            self.history.set(config::search_history());
        }
        self.active.set(ix);
        self.select_card(r.node_index as u32, r.row_index);
        self.query.set(String::new());
    }

    fn set_query(&self, q: String) {
        self.query.set(q);
        self.active.set(0);
    }
}

pub fn page(store: Store) -> impl IntoElement {
    let tree = Tree {
        store,
        query: signal(std::env::var("GRAVIZ_TREE").unwrap_or_default()),
        kind_filter: signal(Vec::new()),
        active: signal(0),
        history: signal(config::search_history()),
        selected: signal(None),
        pinned: signal(None),
        all_types_open: signal(false),
        root_pick: signal(None),
        search_focused: signal(false),
        menu_field: signal(None),
        search_ref: element_ref(),
    };
    crate::ui::arrow_keys(move |d| {
        if !tree.search_focused.get_untracked() || store.palette_open.get_untracked() {
            return;
        }
        let n = tree.model_untracked().map_or(0, |m| {
            results(&m, &tree.query.get_untracked(), &tree.kind_filter.get_untracked()).1.len()
        });
        tree.active.update(|a| *a = (*a as i32 + d).clamp(0, n.saturating_sub(1) as i32) as usize);
    });
    // A new model (another slice, another schema) starts the tree over, but
    // a root that was picked stays picked while the new model declares it:
    // picking one rebuilds through here.
    effect(move || {
        let Some(m) = store.model.get() else { return };
        untracked(|| {
            let names = root_names(&m);
            let pick = tree.root_pick.get_untracked();
            if !pick.as_ref().is_some_and(|p| names.contains(p)) {
                tree.root_pick.set(names.into_iter().next());
            }
            tree.query.set(String::new());
            tree.kind_filter.set(Vec::new());
            tree.selected.set(None);
            tree.pinned.set(None);
            tree.menu_field.set(None);
            tree.active.set(0);
            if let Ok(name) = std::env::var("GRAVIZ_TREE_SEL") {
                if let Some(&c) = m.index_of.get(&name) {
                    tree.selected.set(Some(c));
                    tree.all_types_open.set(true);
                }
            }
        });
    });

    view()
        .flex_1()
        .min_h(px(0.))
        .bg(c(|t| t.panel))
        .on_press_down(move |_: &PressEvent| tree.menu_field.set(None))
        .child(search_box(tree))
        .child(dynamic(move || {
            let focused = tree.search_focused.get();
            let empty = tree.query.with(|q| q.trim().is_empty());
            let has_history = tree.history.with(|h| !h.is_empty());
            if focused && empty && has_history {
                history_list(tree).into_element()
            } else if !empty {
                results_view(tree).into_element()
            } else {
                browse(tree).into_element()
            }
        }))
}

fn search_box(tree: Tree) -> impl IntoElement {
    view().flex_none().px(px(12.0)).py(px(8.0)).child(
        view()
            .w_full()
            .h(px(30.0))
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .border_1()
            .border_color(move || if tree.search_focused.get() { th().accent.hsla() } else { th().card_border.hsla() })
            .bg(c(|t| t.input_bg))
            .child(icon(Icon::Search, 12.0, c(|t| t.text_muted)))
            .child(
                nsh_core::element::input()
                    .element_ref(&tree.search_ref)
                    .flex_1()
                    .min_w(px(0.))
                    .font_family(MONO)
                    .text_size(px(12.0))
                    .text_color(c(|t| t.text))
                    .placeholder("Search types & fields…")
                    .placeholder_color(c(|t| t.text_muted))
                    .caret_color(c(|t| t.accent))
                    .selection_color(c(|t| t.accent.opacity(0.35)))
                    .value(move || tree.query.get())
                    .on_change(move |s: &str| tree.set_query(s.to_string()))
                    .on_focus(move || tree.search_focused.set(true))
                    .on_blur(move || tree.search_focused.set(false))
                    .on_submit(move |_: &str| tree.select_result(tree.active.get_untracked()))
                    .on_key_down(move |e: &KeyEvent| match e.key.as_ref() {
                        "Escape" => {
                            tree.set_query(String::new());
                            tree.search_ref.blur();
                        }
                        _ => {}
                    }),
            )
            .child(dynamic(move || {
                if tree.query.with(String::is_empty) {
                    mono("⌘K", 10.0).flex_none().text_color(c(|t| t.text_muted.opacity(0.5))).into_element()
                } else {
                    view()
                        .flex_none()
                        .cursor(Cursor::Pointer)
                        .on_press(move || tree.set_query(String::new()))
                        .child(icon(Icon::Close, 12.0, c(|t| t.text_muted)))
                        .into_element()
                }
            })),
    )
}

fn history_list(tree: Tree) -> impl IntoElement {
    let mut list = view().child(
        view()
            .flex_row()
            .items_center()
            .justify_between()
            .px(px(12.0))
            .py(px(6.0))
            .child(section_header("recent"))
            .child(
                metro("clear all", 11.0)
                    .text_color(c(|t| t.text_muted))
                    .hover(|s| s.text_color(th().accent.hsla()))
                    .cursor(Cursor::Pointer)
                    .on_press_down(move |_: &PressEvent| {
                        tree.history.set(Vec::new());
                        config::set_search_history(&[]);
                    }),
            ),
    );
    for q in tree.history.get() {
        let (fill, del) = (q.clone(), q.clone());
        list = list.child(
            view()
                .w_full()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(12.0))
                .py(px(6.0))
                .cursor(Cursor::Pointer)
                .hover(|s| s.bg(th().hover_bg.hsla()))
                // On press-down: a release would come after the search box
                // lost focus, and by then this list is gone.
                .on_press_down(move |_: &PressEvent| tree.set_query(fill.clone()))
                .child(icon(Icon::Clock, 12.0, c(|t| t.text_muted)))
                .child(mono(q, 12.0).flex_1().min_w(px(0.)).truncate().text_color(c(|t| t.text)))
                .child(view().flex_none().cursor(Cursor::Pointer).on_press_down(move |_: &PressEvent| {
                    tree.history.update(|h| h.retain(|s| s != &del));
                    config::set_search_history(&tree.history.get_untracked());
                }).child(icon(Icon::Close, 12.0, c(|t| t.text_muted)))),
        );
    }
    scroll().flex_1().min_h(px(0.)).child(list)
}

fn results_view(tree: Tree) -> impl IntoElement {
    dynamic(move || {
        let Some(m) = tree.model() else { return view().into_element() };
        let q = tree.query.get();
        let filter = tree.kind_filter.get();
        let (res, filtered) = results(&m, &q, &filter);
        let mut counts: Vec<(NodeKind, &str, usize)> = Vec::new();
        for (k, l) in KIND_ORDER {
            let n = res.iter().filter(|r| r.type_kind == k).count();
            if n > 0 {
                counts.push((k, l, n));
            }
        }
        let mut chips = view().flex_none().flex_row().flex_wrap().items_center().gap(px(4.0)).px(px(12.0)).py(px(6.0));
        for (kind, label, count) in counts {
            let active = filter.contains(&kind);
            chips = chips.child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .border_1()
                    .px(px(6.0))
                    .h(px(20.0))
                    .cursor(Cursor::Pointer)
                    .border_color(move || if active { th().kind_color(kind).hsla() } else { th().card_border.hsla() })
                    .bg(move || if active { th().kind_color(kind).hsla() } else { Color::CLEAR.hsla() })
                    .on_press(move || {
                        tree.kind_filter.update(|f| {
                            if let Some(i) = f.iter().position(|k| *k == kind) {
                                f.remove(i);
                            } else {
                                f.push(kind);
                            }
                        });
                        tree.active.set(0);
                    })
                    .child(metro(label, 11.0).text_color(move || if active { rgb(0xffffff).into() } else { th().text_muted.hsla() }))
                    .child(mono(count.to_string(), 9.0).text_color(move || if active { rgb(0xffffff).into() } else { th().text_faint.hsla() })),
            );
        }
        if !filter.is_empty() {
            chips = chips.child(
                view()
                    .ml(auto())
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .cursor(Cursor::Pointer)
                    .on_press(move || {
                        tree.kind_filter.set(Vec::new());
                        tree.active.set(0);
                    })
                    .child(icon(Icon::Close, 10.0, c(|t| t.text_muted)))
                    .child(metro("clear", 11.0).text_color(c(|t| t.text_muted))),
            );
        }
        let mut list = view();
        if filtered.is_empty() {
            list = list.child(metro("no results", 12.0).p(px(24.0)).text_center().text_color(c(|t| t.text_muted)));
        }
        for (ix, &ri) in filtered.iter().enumerate() {
            list = list.child(result_row(tree, ix, &res[ri]));
        }
        let mut col = view().flex_1().min_h(px(0.));
        if !res.is_empty() {
            col = col.child(chips);
        }
        col.child(scroll().flex_1().min_h(px(0.)).child(list)).into_element()
    })
}

fn result_row(tree: Tree, ix: usize, r: &SearchResult) -> impl IntoElement {
    let selected = move || tree.active.get() == ix;
    let primary = c(|t| t.accent);
    let name = match &r.field_name {
        Some(field) => text("")
            .child(highlighted(&r.type_name, r.type_match_indices.as_deref().unwrap_or(&[]), primary).text_color(c(|t| t.text_muted)))
            .child(text(".").text_color(c(|t| t.text_muted)))
            .child(highlighted(field, &r.match_indices, primary).text_color(c(|t| t.text))),
        None => highlighted(&r.type_name, &r.match_indices, primary).text_color(c(|t| t.text)),
    };
    let line1 = view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(kind_badge(r.type_kind, kind_label(r.type_kind)))
        .child(name.font_family(MONO).text_size(px(12.0)).flex_1().min_w(px(0.)).truncate())
        .child(when(r.field_type.is_some(), {
            let ft = r.field_type.clone().unwrap_or_default();
            move || mono(ft.clone(), 10.0).flex_none().text_color(c(|t| t.text_muted))
        }));
    let line2 = r.snippet.as_ref().map(|s| {
        let deprecated = r.snippet_kind == Some(SnippetKind::DeprecationReason);
        view()
            .flex_row()
            .min_w(px(0.))
            .items_center()
            .gap(px(4.0))
            .pl(px(4.0))
            .child(
                view()
                    .flex_none()
                    .px(px(4.0))
                    .py(px(1.0))
                    .bg(move || if deprecated { th().type_amber.opacity(0.15).hsla() } else { th().active_bg.hsla() })
                    .child(metro(if deprecated { "deprecated" } else { "desc" }, 9.0).text_color(move || {
                        if deprecated { th().type_amber.hsla() } else { th().text_muted.hsla() }
                    })),
            )
            .child(
                highlighted(&s.snippet, &s.indices, primary)
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .italic()
                    .font_family(MONO)
                    .text_size(px(10.0))
                    .text_color(c(|t| t.text_muted)),
            )
    });
    view()
        .w_full()
        .gap(px(2.0))
        .px(px(12.0))
        .py(px(6.0))
        .cursor(Cursor::Pointer)
        .border_l(px(2.0))
        .border_color(move || if selected() { th().accent.hsla() } else { Color::CLEAR.hsla() })
        .bg(move || if selected() { th().active_bg.hsla() } else { Color::CLEAR.hsla() })
        .hover(|s| s.bg(th().hover_bg.hsla()))
        .on_press(move || tree.select_result(ix))
        .child(line1)
        .child(when(line2.is_some(), {
            let line2 = std::cell::RefCell::new(line2);
            move || line2.borrow_mut().take().map(IntoElement::into_element).unwrap_or_else(|| view().into_element())
        }))
}

fn section_header(title: &'static str) -> impl IntoElement {
    nsh_metro::text(TextStyle::Normal, title).color(move || AdaptiveColor::from(th().text.hsla()))
}

fn section_count(count: usize) -> impl IntoElement {
    metro(count.to_string(), 10.0).self_start().mt(px(1.0)).font_weight(SEMIBOLD).text_color(c(|t| t.text_muted))
}

/// Roots, all types, the selection's sections and its detail.
fn browse(tree: Tree) -> impl IntoElement {
    dynamic(move || {
        let Some(m) = tree.model() else { return view().into_element() };
        let selected = tree.selected.get();
        let mut col = view().flex_1().min_h(px(0.)).child(roots(tree, &m)).child(all_types(tree, &m));
        if let Some(card) = selected {
            let s = sections(&m, card);
            for (title, list) in [("implemented by", s.implementers), ("members", s.union_members), ("referenced by", s.referenced_by)] {
                if !list.is_empty() {
                    col = col.child(context_section(tree, &m, title, list));
                }
            }
        }
        col.child(detail(tree, &m, selected)).into_element()
    })
}

fn roots(tree: Tree, m: &Model) -> impl IntoElement {
    let label = if m.schema_name.is_empty() { "Schema".to_string() } else { m.schema_name.clone() };
    let mut buttons = view().flex_row().flex_wrap().gap(px(4.0));
    for (op, name) in m.root_ops.clone() {
        let pick = name.to_string();
        let active = {
            let pick = pick.clone();
            move || tree.root_pick.get().as_deref() == Some(pick.as_str())
        };
        let a2 = active.clone();
        let a3 = active.clone();
        let a4 = active.clone();
        buttons = buttons.child(
            view()
                .justify_end()
                .gap(px(1.0))
                .min_w(px(84.0))
                .h(px(52.0))
                .px(px(8.0))
                .pb(px(4.0))
                .cursor(Cursor::Pointer)
                .bg(move || if a4() { th().accent.hsla() } else { th().hover_bg.hsla() })
                .hover(|s| s.opacity(0.9))
                .on_press({
                    let pick = pick.clone();
                    move || {
                        tree.root_pick.set(Some(pick.clone()));
                        tree.store.root_override.set(Some(pick.clone()));
                        tree.store.rebuild();
                    }
                })
                .child(metro(op, 10.0).opacity(move || if a2() { 0.85_f32 } else { 0.6 }).text_color(move || {
                    if a3() { nsh_metro::palette().accent_ink } else { th().text.hsla() }
                }))
                .child(mono(name.to_string(), 12.0).text_color({
                    let active = active.clone();
                    move || if active() { nsh_metro::palette().accent_ink } else { th().text.hsla() }
                })),
        );
    }
    let mut col = view()
        .flex_none()
        .px(px(12.0))
        .pt(px(4.0))
        .pb(px(12.0))
        .child(metro(label.to_uppercase(), 10.0).mb(px(6.0)).font_weight(SEMIBOLD).text_color(c(|t| t.text_muted)))
        .child(buttons);
    if m.root_ops.is_empty() {
        col = col.child(metro("No root operations — pick a type below to explore from.", 11.0).text_color(c(|t| t.text_muted)));
    }
    col
}

fn type_row(tree: Tree, m: &Model, card: u32, chips: &[S]) -> impl IntoElement {
    let c_ = &m.cards[card as usize];
    let selected = move || tree.selected.get() == Some(card);
    let kind = c_.kind;
    let mut chip_row = view().ml(auto()).flex_row().flex_none().items_center().gap(px(4.0));
    for label in chips {
        chip_row = chip_row.child(
            view()
                .flex_none()
                .px(px(4.0))
                .py(px(1.0))
                .bg(move || if selected() { Color::WHITE.opacity(0.2).hsla() } else { th().active_bg.hsla() })
                .child(metro(label.to_string(), 9.0).text_color(move || if selected() { rgb(0xffffff).into() } else { th().text_muted.hsla() })),
        );
    }
    let name = c_.name.to_string();
    let menu = nsh_metro::context_menu()
        .item("show in graph", move || tree.select_card(card, None))
        .item("copy name", move || set_clipboard(name.clone()));
    let row = view()
        .w_full()
        .h(px(LIST_ROW_H))
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(4.0))
        .cursor(Cursor::Pointer)
        .bg(move || if selected() { th().accent.hsla() } else { Color::CLEAR.hsla() })
        .hover(|s| s.bg(th().hover_bg.hsla()))
        .on_press(move || tree.select_card(card, None))
        .child(
            view()
                .flex_none()
                .px(px(6.0))
                .bg(move || if selected() { Color::WHITE.opacity(0.2).hsla() } else { th().kind_color(kind).hsla() })
                .child(text(c_.kind_label).text_size(px(9.0)).line_height(px(14.0)).text_color(rgb(0xffffff))),
        )
        .child(mono(c_.name.to_string(), 12.0).flex_1().min_w(px(0.)).truncate().text_color(move || {
            if selected() { rgb(0xffffff).into() } else { th().text.hsla() }
        }))
        .child(chip_row);
    nsh_metro::with_context_menu(row, menu)
}

fn bounded_list(rows: usize) -> Scroll {
    scroll().flex_none().h(px((rows as f32 * LIST_ROW_H).min(LIST_MAX_H)))
}

fn all_types(tree: Tree, m: &Shared<Model>) -> impl IntoElement {
    let all = sorted_cards(m);
    if all.is_empty() {
        return view().into_element();
    }
    let count = all.len();
    let m2 = m.clone();
    view()
        .flex_none()
        .child(
            view()
                .w_full()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(12.0))
                .py(px(4.0))
                .cursor(Cursor::Pointer)
                .hover(|s| s.bg(th().hover_bg.hsla()))
                .on_press(move || tree.all_types_open.update(|o| *o = !*o))
                .child(dynamic(move || {
                    icon(if tree.all_types_open.get() { Icon::ChevronDown } else { Icon::ChevronRight }, 12.0, c(|t| t.text_muted))
                        .into_element()
                }))
                .child(section_header("all types"))
                .child(section_count(count)),
        )
        .child(when(move || tree.all_types_open.get(), move || {
            let mut list = view();
            for &card in &all {
                list = list.child(type_row(tree, &m2, card, &[]));
            }
            bounded_list(count).child(list)
        }))
        .into_element()
}

fn context_section(tree: Tree, m: &Model, title: &'static str, list: Vec<(u32, Vec<S>)>) -> impl IntoElement {
    let count = list.len();
    let mut rows = view();
    for (card, chips) in &list {
        rows = rows.child(type_row(tree, m, *card, chips));
    }
    view()
        .flex_none()
        .child(
            view()
                .w_full()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .px(px(12.0))
                .pt(px(8.0))
                .pb(px(4.0))
                .child(section_header(title))
                .child(section_count(count)),
        )
        .child(bounded_list(count).child(rows))
}

fn detail(tree: Tree, m: &Model, selected: Option<u32>) -> impl IntoElement {
    let Some(card) = selected else {
        return scroll()
            .flex_1()
            .min_h(px(0.))
            .child(metro("select a type to start exploring", 12.0).p(px(24.0)).text_center().text_color(c(|t| t.text_muted)))
            .into_element();
    };
    let c_ = &m.cards[card as usize];
    let mut pane = view().p(px(12.0)).child(
        view()
            .mb(px(8.0))
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                view()
                    .flex_none()
                    .px(px(8.0))
                    .py(px(2.0))
                    .bg({
                        let k = c_.kind;
                        move || th().kind_color(k).hsla()
                    })
                    .child(text(c_.kind_label).text_size(px(12.0)).text_color(rgb(0xffffff))),
            )
            .child(mono(c_.name.to_string(), 18.0).flex_1().min_w(px(0.)).truncate().text_color(c(|t| t.text))),
    );
    if let Some(d) = c_.description.clone() {
        pane = pane.child(metro(d, 12.0).mb(px(12.0)).text_color(c(|t| t.text_muted)));
    }
    if !c_.implements.is_empty() {
        let mut row = view()
            .mb(px(12.0))
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(4.0))
            .child(metro("implements", 11.0).text_color(c(|t| t.text_muted)));
        for name in &c_.implements {
            let navigable = !BUILTIN.contains(&name.as_ref()) && m.index_of.contains_key(name.as_ref());
            let target = m.index_of.get(name.as_ref()).copied();
            let mut b = view().px(px(6.0)).py(px(2.0)).child(mono(name.to_string(), 11.0).text_color(c(|t| t.text_muted)));
            if navigable {
                b = b.cursor(Cursor::Pointer).bg(c(|t| t.active_bg)).hover(|s| s.bg(th().hover_bg.hsla())).on_press(move || {
                    if let Some(t) = target {
                        tree.select_card(t, None);
                    }
                });
            } else {
                b = b.opacity(0.6);
            }
            row = row.child(b);
        }
        pane = pane.child(row);
    }
    let mut body = view().gap(px(2.0));
    if c_.rows.is_empty() {
        body = body.child(
            mono(if c_.kind == NodeKind::Enum { "no values" } else { "no fields" }, 12.0)
                .px(px(8.0))
                .py(px(4.0))
                .italic()
                .text_color(c(|t| t.text_muted)),
        );
    } else {
        for dix in 0..c_.rows.len() {
            body = body.child(field_row(tree, m, card, dix));
        }
    }
    scroll().flex_1().min_h(px(0.)).child(pane.child(body)).into_element()
}

fn field_row(tree: Tree, m: &Model, card: u32, dix: usize) -> impl IntoElement {
    let c_ = &m.cards[card as usize];
    let row = &c_.rows[dix];
    let expired = row.until_expired;
    let deprecated = row.deprecated;
    let is_member = row.kind == RowKind::UnionMember;
    let target = row.target;
    let graph_row = c_.row_map.iter().position(|&x| x == Some(dix as u32));
    let pinned = move || tree.pinned.get() == Some((card, dix));
    let label = if is_member { format!("| {}", row.left) } else { row.left.to_string() };
    let mut name = mono(label, 12.0).min_w(px(0.)).truncate().text_color(move || if expired { th().red.hsla() } else { th().text.hsla() });
    if deprecated {
        name = name.line_through();
    }
    let mut left = view().flex_row().min_w(px(0.)).items_center().gap(px(4.0)).child(name);
    if !row.args.is_empty() {
        left = left.child(mono(format!("({}/{})", row.required_args, row.args.len()), 10.0).flex_none().text_color(c(|t| t.text_muted)));
    }
    let mut chip = view().flex_row().min_w(px(0.)).items_center().gap(px(2.0));
    if row.is_relay {
        chip = chip.child(glyph(Glyph::Link2, 10.0, c(|t| t.relay_orange)));
    }
    if !row.right.is_empty() {
        chip = chip.child(mono(row.right.to_string(), 12.0).min_w(px(0.)).truncate().text_color(c(|t| t.type_amber)));
    }
    if target.is_some() {
        chip = chip
            .cursor(Cursor::Pointer)
            .hover(|s| s.bg(th().accent.opacity(0.15).hsla()))
            .on_press(move || {
                if let Some(t) = target {
                    tree.select_card(t, None);
                }
            })
            .child(icon(Icon::ChevronRight, 12.0, c(|t| t.text_muted)));
    }
    let line1 = view()
        .w_full()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(left)
        .child(view().flex_row().min_w(px(0.)).items_center().justify_end().child(chip));
    let mut el = view()
        .relative()
        .gap(px(2.0))
        .px(px(8.0))
        .py(px(4.0))
        .border_1()
        .border_color(move || if pinned() { th().pin.hsla() } else { Color::CLEAR.hsla() })
        .bg(move || {
            let t = th();
            if pinned() {
                t.pin.opacity(0.1).hsla()
            } else if expired {
                t.red.opacity(0.1).hsla()
            } else if deprecated {
                t.type_amber.opacity(0.1).hsla()
            } else {
                Color::CLEAR.hsla()
            }
        })
        .opacity(move || if !pinned() && !expired && deprecated { 0.6_f32 } else { 1.0 })
        .cursor(Cursor::Pointer)
        .hover(|s| s.bg(th().hover_bg.hsla()))
        .on_press(move || {
            if is_member {
                if let Some(t) = target {
                    tree.select_card(t, None);
                }
            } else {
                tree.select_card(card, graph_row);
            }
        })
        .on_context_menu(move |_: &ContextMenuEvent| tree.menu_field.set(Some((card, dix))))
        .child(line1);
    if deprecated {
        let text_: String = row.deprecation_reason.clone().unwrap_or_else(|| "Deprecated".into());
        let tone = move || if expired { th().red.hsla() } else { th().type_amber.hsla() };
        el = el.child(
            view()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(icon(Icon::Alert, 10.0, tone))
                .child(metro(text_, 11.0).text_color(tone)),
        );
    }
    if let Some(d) = row.description.clone() {
        el = el.child(metro(d, 11.0).text_color(c(|t| t.text_muted)));
    }
    let path = format!("{}.{}", c_.name, row.left);
    el.child(when(move || tree.menu_field.get() == Some((card, dix)), move || {
        let path = path.clone();
        view()
            .absolute()
            .top(px(28.0))
            .right(px(4.0))
            .min_w(px(180.0))
            .bg(c(|t| t.chrome_bg))
            .py(px(4.0))
            .z_index(10)
            .child(
                metro(format!("copy \"{path}\""), 14.0)
                    .px(px(12.0))
                    .py(px(6.0))
                    .text_color(c(|t| t.text))
                    .hover(|s| s.bg(th().hover_bg.hsla()))
                    .cursor(Cursor::Pointer)
                    .on_press(move || {
                        set_clipboard(path.clone());
                        tree.menu_field.set(None);
                    }),
            )
    }))
}
