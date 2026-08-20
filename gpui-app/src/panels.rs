//! The two non-tree sidebar bodies: `OrphanPanel` (Orphaned tab) and
//! `UntilPanel` (Deprecated tab).
//!
//! Ports of the components of the same name in the web app's
//! `src/routes/View.tsx`. The orphan list groups types no root operation can
//! reach, by kind; the until list splits deprecated members into expired /
//! upcoming / undated sections, grouped by owning type. Both emit
//! [`PanelEvent::Select`] with the same payload the tree's `TreeEvent` carries,
//! so the workspace can drive the canvas the same way for all three tabs.

// Both views are constructed by `workspace.rs`; until that wiring lands the
// compiler sees the whole public surface as unreachable.
#![allow(dead_code)]

use crate::model::{today_string, Model};
use crate::theme::Theme;
use graviz_core::graph::{
    all_reachable_ids, default_root_ops, is_until_expired, parse_until, NodeKind,
};
use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, Context, EventEmitter, FontWeight, Hsla,
    SharedString, UniformListScrollHandle, Window,
};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A row click. `row` is a graph-space member index (field, then enum value),
/// which the workspace maps through `Card::display_row`.
pub enum PanelEvent {
    Select { node_index: usize, row: Option<usize> },
}

/// The web's `KIND_ORDER` — the fixed order kind groups are listed in.
const KIND_ORDER: [NodeKind; 6] = [
    NodeKind::Object,
    NodeKind::Interface,
    NodeKind::Union,
    NodeKind::Enum,
    NodeKind::Input,
    NodeKind::Scalar,
];

/// Row pitch of the orphan list (the web's `py-1.5` type button).
const ORPHAN_ROW_H: f32 = 28.0;
/// Row pitch of the until list (the web's two-line `py-1.5` field button).
const UNTIL_ROW_H: f32 = 44.0;

/// The kind word a group header paints (`uppercase` in the web's CSS, which
/// GPUI has no equivalent for — the text is uppercased here instead).
fn kind_upper(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Object => "OBJECT",
        NodeKind::Interface => "INTERFACE",
        NodeKind::Union => "UNION",
        NodeKind::Enum => "ENUM",
        NodeKind::Scalar => "SCALAR",
        NodeKind::Input => "INPUT",
    }
}

/// The web's `KIND_STYLES[kind].badge`: a solid kind-colored pill.
fn kind_badge(th: Theme, kind: NodeKind) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(6.0))
        .rounded(px(8.0))
        .bg(th.kind_color(kind))
        .text_size(px(9.0))
        .line_height(px(16.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(gpui::white())
        .child(SharedString::from(kind.sdl_keyword()))
}

/// Root operation names of `graph`, falling back to the conventional trio when
/// the schema declares none (the web's `rootOps`).
fn root_ops_of(graph: &graviz_core::graph::ParsedGraph) -> HashSet<String> {
    let rt = &graph.root_types;
    let set: HashSet<String> = [rt.query.clone(), rt.mutation.clone(), rt.subscription.clone()]
        .into_iter()
        .flatten()
        .collect();
    if set.is_empty() {
        default_root_ops()
    } else {
        set
    }
}

// ---------------------------------------------------------------- orphan tab

enum OrphanRow {
    /// A kind group header: `OBJECT (12)`.
    Kind { label: SharedString, count: SharedString },
    Type {
        card: u32,
        kind: NodeKind,
        name: SharedString,
        /// `12f`, present only for kinds that have a field list.
        fields: Option<SharedString>,
    },
}

pub struct OrphanPanel {
    rows: Vec<OrphanRow>,
    /// Number of orphaned types (the web's `nodes.length`).
    total: usize,
    /// Card index of the last clicked type (the web's `focusId`).
    selected: Option<u32>,
    scroll: UniformListScrollHandle,
}

impl OrphanPanel {
    pub fn new(model: Rc<Model>, _cx: &mut Context<Self>) -> Self {
        let (rows, total) = build_orphan_rows(&model);
        Self { rows, total, selected: None, scroll: UniformListScrollHandle::new() }
    }

    /// Swap in a different slice of the schema (mode change).
    pub fn set_model(&mut self, model: Rc<Model>, cx: &mut Context<Self>) {
        let (rows, total) = build_orphan_rows(&model);
        self.rows = rows;
        self.total = total;
        self.selected = None;
        cx.notify();
    }

    fn select(&mut self, card: u32, cx: &mut Context<Self>) {
        self.selected = Some(card);
        cx.emit(PanelEvent::Select { node_index: card as usize, row: None });
        cx.notify();
    }

    fn render_row(&self, ix: usize, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        match &self.rows[ix] {
            OrphanRow::Kind { label, count } => div()
                .w_full()
                .h(px(ORPHAN_ROW_H))
                .flex()
                .items_center()
                .gap_1()
                .px_3()
                .text_size(px(10.0))
                .text_color(th.text_muted)
                .child(label.clone())
                .child(div().opacity(0.6).child(count.clone()))
                .into_any_element(),
            OrphanRow::Type { card, kind, name, fields } => {
                let card = *card;
                div()
                    .id(ix)
                    .w_full()
                    .h(px(ORPHAN_ROW_H))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .cursor_pointer()
                    .when(self.selected == Some(card), |el| el.bg(th.active_bg))
                    .hover(|el| el.bg(th.hover_bg))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(card, cx)))
                    .child(kind_badge(th, *kind))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family("Menlo")
                            .text_size(px(12.0))
                            .text_color(th.text)
                            .child(name.clone()),
                    )
                    .when_some(fields.clone(), |el, f| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(10.0))
                                .text_color(th.text_faint)
                                .child(f),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

/// Groups the types no root can reach by kind, in `KIND_ORDER`, names sorted.
///
/// Reachability is computed from the model that was handed in: when the
/// workspace already sliced the graph down to the orphans there is no root
/// operation left in it, so every node comes back unreachable and the whole
/// slice is listed.
fn build_orphan_rows(model: &Model) -> (Vec<OrphanRow>, usize) {
    let root_ops = root_ops_of(&model.graph);
    let reachable = all_reachable_ids(&model.graph.nodes, &model.graph.edges, &root_ops);

    let mut by_kind: HashMap<NodeKind, Vec<u32>> = HashMap::new();
    let mut total = 0usize;
    for (i, node) in model.graph.nodes.iter().enumerate() {
        if reachable.contains(&node.id) {
            continue;
        }
        total += 1;
        by_kind.entry(node.kind).or_default().push(i as u32);
    }

    let mut rows = Vec::with_capacity(total + KIND_ORDER.len());
    for kind in KIND_ORDER {
        let Some(list) = by_kind.get_mut(&kind) else { continue };
        list.sort_by(|&a, &b| model.cards[a as usize].name.cmp(&model.cards[b as usize].name));
        rows.push(OrphanRow::Kind {
            label: SharedString::from(kind_upper(kind)),
            count: SharedString::from(format!("({})", list.len())),
        });
        for &card in list.iter() {
            let node = &model.graph.nodes[card as usize];
            rows.push(OrphanRow::Type {
                card,
                kind,
                name: model.cards[card as usize].name.clone(),
                fields: node
                    .fields
                    .as_ref()
                    .map(|f| SharedString::from(format!("{}f", f.len()))),
            });
        }
    }
    (rows, total)
}

impl EventEmitter<PanelEvent> for OrphanPanel {}

impl Render for OrphanPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        let base = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(th.panel)
            .border_r_1()
            .border_color(th.panel_border);

        if self.total == 0 {
            return base.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(24.0))
                    .text_size(px(12.0))
                    .text_color(th.text_muted)
                    .child(SharedString::from("No orphaned types found.")),
            );
        }

        let total = self.total;
        base.child(
            div()
                .px_3()
                .py_2()
                .text_size(px(10.0))
                .text_color(th.text_muted)
                .child(SharedString::from(format!(
                    "{total} type{} unreachable from root",
                    if total == 1 { "" } else { "s" }
                ))),
        )
        .child(
            uniform_list(
                "orphan-items",
                self.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                    let th = crate::theme::current(cx, window.appearance());
                    range.map(|ix| this.render_row(ix, th, cx)).collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .track_scroll(&self.scroll),
        )
    }
}

// ------------------------------------------------------------ deprecated tab

/// The web's `UntilVariant` — one per section of the deprecated list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Expired,
    Upcoming,
    Undated,
}

impl Variant {
    /// Section title, already uppercased (the web applies `uppercase` in CSS).
    fn title(self) -> &'static str {
        match self {
            Variant::Expired => "EXPIRED",
            Variant::Upcoming => "UPCOMING",
            Variant::Undated => "DEPRECATED",
        }
    }

    /// Banner text and field-name color.
    fn text(self, th: Theme) -> Hsla {
        match self {
            Variant::Expired => th.red,
            Variant::Upcoming => th.type_amber,
            Variant::Undated => th.text_muted,
        }
    }

    /// The 10%-tinted banner background, reused as the selected-row wash.
    fn tint(self, th: Theme) -> Hsla {
        match self {
            Variant::Expired => th.expired.opacity(0.1),
            Variant::Upcoming => th.type_amber.opacity(0.1),
            Variant::Undated => th.text_muted.opacity(0.1),
        }
    }

    /// Selected-row background (the web's `focus` class).
    fn focus(self, th: Theme) -> Hsla {
        match self {
            Variant::Undated => th.active_bg,
            _ => self.tint(th),
        }
    }

    /// Color of the trailing `YYYY-MM-DD` chip.
    fn date(self, th: Theme) -> Hsla {
        match self {
            Variant::Expired => th.expired,
            _ => th.type_amber,
        }
    }
}

enum UntilRow {
    Banner { variant: Variant, count: SharedString },
    /// Owning-type header: badge + type name + member count.
    Group { kind: NodeKind, name: SharedString, count: SharedString },
    Field {
        variant: Variant,
        card: u32,
        row: usize,
        name: SharedString,
        date: Option<SharedString>,
        meta: SharedString,
    },
}

/// One deprecated member, before it is grouped into sections.
struct Entry {
    card: u32,
    row: usize,
    name: SharedString,
    type_name: SharedString,
    kind: NodeKind,
    until: Option<String>,
    reason: Option<String>,
}

pub struct UntilPanel {
    rows: Vec<UntilRow>,
    /// Card index of the last clicked field's type (the web's `focusId`, which
    /// tints every row belonging to that type).
    selected: Option<u32>,
    scroll: UniformListScrollHandle,
}

impl UntilPanel {
    pub fn new(model: Rc<Model>, _cx: &mut Context<Self>) -> Self {
        Self {
            rows: build_until_rows(&model, &today_string()),
            selected: None,
            scroll: UniformListScrollHandle::new(),
        }
    }

    /// Swap in a different slice of the schema (mode change).
    pub fn set_model(&mut self, model: Rc<Model>, cx: &mut Context<Self>) {
        self.rows = build_until_rows(&model, &today_string());
        self.selected = None;
        cx.notify();
    }

    fn select(&mut self, card: u32, row: usize, cx: &mut Context<Self>) {
        self.selected = Some(card);
        cx.emit(PanelEvent::Select { node_index: card as usize, row: Some(row) });
        cx.notify();
    }

    fn render_row(&self, ix: usize, th: Theme, cx: &mut Context<Self>) -> AnyElement {
        match &self.rows[ix] {
            UntilRow::Banner { variant, count } => {
                let variant = *variant;
                div()
                    .w_full()
                    .h(px(UNTIL_ROW_H))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .px_3()
                            .py(px(6.0))
                            .bg(variant.tint(th))
                            .text_size(px(10.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(variant.text(th))
                            .child(SharedString::from(variant.title()))
                            .child(div().opacity(0.7).child(count.clone())),
                    )
                    .into_any_element()
            }
            UntilRow::Group { kind, name, count } => div()
                .w_full()
                .h(px(UNTIL_ROW_H))
                .flex()
                .items_center()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(px(6.0))
                        .px_3()
                        .py_1()
                        .text_size(px(10.0))
                        .child(kind_badge(th, *kind))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family("Menlo")
                                .text_color(th.text_muted)
                                .child(name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .opacity(0.6)
                                .text_color(th.text)
                                .child(count.clone()),
                        ),
                )
                .into_any_element(),
            UntilRow::Field { variant, card, row, name, date, meta } => {
                let (variant, card, row) = (*variant, *card, *row);
                div()
                    .id(ix)
                    .w_full()
                    .h(px(UNTIL_ROW_H))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap(px(2.0))
                    .px_3()
                    .cursor_pointer()
                    .when(self.selected == Some(card), |el| el.bg(variant.focus(th)))
                    .hover(|el| el.bg(th.hover_bg))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(card, row, cx)))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_family("Menlo")
                            .text_size(px(12.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .line_through()
                                    .text_color(variant.text(th))
                                    .child(name.clone()),
                            )
                            .when_some(date.clone(), |el, d| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .font_family("Menlo")
                                        .text_size(px(10.0))
                                        .text_color(variant.date(th))
                                        .child(d),
                                )
                            }),
                    )
                    .child(
                        div()
                            .w_full()
                            .truncate()
                            .text_size(px(10.0))
                            .text_color(th.text_muted)
                            .child(meta.clone()),
                    )
                    .into_any_element()
            }
        }
    }
}

/// Scans every type for deprecated members and splits them into the three
/// sections, each grouped by owning type (the web's `expiredFields` /
/// `upcomingFields` / `deprecatedFields` plus `groupByType`).
fn build_until_rows(model: &Model, today: &str) -> Vec<UntilRow> {
    let (mut expired, mut upcoming, mut undated) = (Vec::new(), Vec::new(), Vec::new());
    for (i, node) in model.graph.nodes.iter().enumerate() {
        // Enums carry their deprecations on values, everything else on fields.
        // Both are addressed by the same graph-space row index.
        let members: Vec<(&str, Option<&str>, bool)> = if node.kind == NodeKind::Enum {
            node.values
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|v| (v.name.as_str(), v.deprecation_reason.as_deref(), v.is_deprecated))
                .collect()
        } else {
            node.fields
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|f| (f.name.as_str(), f.deprecation_reason.as_deref(), f.is_deprecated))
                .collect()
        };
        for (row, (name, reason, is_deprecated)) in members.into_iter().enumerate() {
            let until = parse_until(reason);
            if until.is_none() && !is_deprecated {
                continue;
            }
            let entry = Entry {
                card: i as u32,
                row,
                name: SharedString::from(name.to_string()),
                type_name: model.cards[i].name.clone(),
                kind: node.kind,
                until: until.clone(),
                reason: reason.map(str::to_string),
            };
            match until {
                None => undated.push(entry),
                Some(u) if is_until_expired(Some(&u), today) => expired.push(entry),
                Some(_) => upcoming.push(entry),
            }
        }
    }

    // Dated sections sort by date (most overdue / soonest first), the undated
    // one by type then member name.
    expired.sort_by(|a: &Entry, b: &Entry| a.until.cmp(&b.until));
    upcoming.sort_by(|a: &Entry, b: &Entry| a.until.cmp(&b.until));
    undated.sort_by(|a: &Entry, b: &Entry| {
        a.type_name.cmp(&b.type_name).then_with(|| a.name.cmp(&b.name))
    });

    let mut rows = Vec::new();
    push_section(&mut rows, Variant::Expired, expired, today);
    push_section(&mut rows, Variant::Upcoming, upcoming, today);
    push_section(&mut rows, Variant::Undated, undated, today);
    rows
}

/// Appends one section: its banner, then a header + field rows per owning
/// type, in first-seen order.
fn push_section(rows: &mut Vec<UntilRow>, variant: Variant, entries: Vec<Entry>, today: &str) {
    if entries.is_empty() {
        return;
    }
    rows.push(UntilRow::Banner {
        variant,
        count: SharedString::from(format!("({})", entries.len())),
    });

    let mut order: Vec<Vec<Entry>> = Vec::new();
    let mut at: HashMap<u32, usize> = HashMap::new();
    for entry in entries {
        match at.get(&entry.card) {
            Some(&i) => order[i].push(entry),
            None => {
                at.insert(entry.card, order.len());
                order.push(vec![entry]);
            }
        }
    }

    for group in order {
        let head = &group[0];
        rows.push(UntilRow::Group {
            kind: head.kind,
            name: head.type_name.clone(),
            count: SharedString::from(format!("({})", group.len())),
        });
        for entry in group {
            let meta = meta_line(variant, entry.until.as_deref(), entry.reason.as_deref(), today);
            rows.push(UntilRow::Field {
                variant,
                card: entry.card,
                row: entry.row,
                name: entry.name,
                date: entry.until.map(SharedString::from),
                meta: SharedString::from(meta),
            });
        }
    }
}

/// The second line of a field row: how far past (or short of) the sunset date
/// the field is, joined with its deprecation reason.
fn meta_line(variant: Variant, until: Option<&str>, reason: Option<&str>, today: &str) -> String {
    if variant == Variant::Undated {
        return reason.unwrap_or("deprecated").to_string();
    }
    let days = days_from_now(until.unwrap_or_default(), today);
    let when = if variant == Variant::Expired {
        if days > 0 {
            format!("{}d overdue", group_digits(days))
        } else {
            "overdue".to_string()
        }
    } else if days < 0 {
        format!("in {}d", group_digits(-days))
    } else {
        "due today".to_string()
    };
    match reason {
        Some(r) => format!("{when} · {}", strip_until(r)),
        None => when,
    }
}

/// Whole days between the end of the `until` day and `today`. Positive →
/// overdue by that many days; negative → that many days left.
///
/// Port of the web's `daysFromNow`, which measures against `until`'s
/// `23:59:59.999`: a date whose day is only just over is 0 days overdue. Both
/// sides are day-granularity here, so the sub-day remainder always floors to
/// one extra day. Unparsable input yields 0, like the TS's `isFinite` guard.
fn days_from_now(until: &str, today: &str) -> i64 {
    match (days_from_civil(until), days_from_civil(today)) {
        (Some(u), Some(t)) => t - u - 1,
        _ => 0,
    }
}

/// `YYYY-MM-DD` → days since the Unix epoch (Howard Hinnant's
/// `days_from_civil`, the inverse of the one in `model::today_string`).
fn days_from_civil(date: &str) -> Option<i64> {
    if !is_iso_shape(date) {
        return None;
    }
    let y: i64 = date[0..4].parse().ok()?;
    let m: i64 = date[5..7].parse().ok()?;
    let d: i64 = date[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `\d{4}-\d{2}-\d{2}` — shape only, no calendar validation.
fn is_iso_shape(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// `20683` → `"20,683"` — the web formats day counts with `toLocaleString`.
fn group_digits(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Drops the `[until YYYY-MM-DD]` marker from a reason — the date already has
/// its own chip. Port of the web's `stripUntil`, which replaces the leftmost
/// match (plus the whitespace after it) and trims what is left.
fn strip_until(reason: &str) -> String {
    for (open, _) in reason.as_bytes().iter().enumerate().filter(|(_, b)| **b == b'[') {
        if let Some(end) = until_marker_end(reason, open) {
            let mut out = String::with_capacity(reason.len());
            out.push_str(&reason[..open]);
            out.push_str(&reason[end..]);
            return out.trim().to_string();
        }
    }
    reason.trim().to_string()
}

/// Matches `\[\s*until\s+\d{4}-\d{2}-\d{2}\s*\]\s*` at `open`, returning the
/// byte offset just past it.
fn until_marker_end(s: &str, open: usize) -> Option<usize> {
    let i = skip_whitespace(s, open + 1);
    if !s.get(i..i + 5)?.eq_ignore_ascii_case("until") {
        return None;
    }
    let j = skip_whitespace(s, i + 5);
    if j == i + 5 {
        return None; // `\s+` — at least one
    }
    // Shape only, like the TS regex — a nonsense calendar date still counts
    // as a marker and still gets dropped.
    let date = s.get(j..j + 10)?;
    if !is_iso_shape(date) {
        return None;
    }
    let k = skip_whitespace(s, j + 10);
    if s.as_bytes().get(k) != Some(&b']') {
        return None;
    }
    Some(skip_whitespace(s, k + 1))
}

fn skip_whitespace(s: &str, from: usize) -> usize {
    let mut i = from;
    loop {
        let Some(rest) = s.get(i..) else { return i };
        match rest.chars().next() {
            Some(c) if c.is_whitespace() => i += c.len_utf8(),
            _ => return i,
        }
    }
}

impl EventEmitter<PanelEvent> for UntilPanel {}

impl Render for UntilPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = crate::theme::current(cx, window.appearance());
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(th.panel)
            .border_r_1()
            .border_color(th.panel_border)
            .child(
                uniform_list(
                    "until-items",
                    self.rows.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                        let th = crate::theme::current(cx, window.appearance());
                        range.map(|ix| this.render_row(ix, th, cx)).collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .track_scroll(&self.scroll),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_counts_days_since_the_epoch() {
        assert_eq!(days_from_civil("1970-01-01"), Some(0));
        assert_eq!(days_from_civil("1970-01-02"), Some(1));
        assert_eq!(days_from_civil("1969-12-31"), Some(-1));
        // 2000 is a leap year, so March starts 60 days into it.
        assert_eq!(days_from_civil("2000-03-01"), Some(11017));
        assert_eq!(days_from_civil("2026-08-19"), Some(20684));
    }

    #[test]
    fn days_from_civil_rejects_malformed_dates() {
        assert_eq!(days_from_civil(""), None);
        assert_eq!(days_from_civil("2026-8-19"), None);
        assert_eq!(days_from_civil("2026/08/19"), None);
        assert_eq!(days_from_civil("2026-13-01"), None);
        assert_eq!(days_from_civil("2026-01-00"), None);
        assert_eq!(days_from_civil("not-a-date"), None);
    }

    #[test]
    fn days_from_now_measures_against_the_end_of_the_until_day() {
        // A sunset date that only just passed is 0 days overdue, not 1 — the
        // marker means "end of that calendar day".
        assert_eq!(days_from_now("2026-08-18", "2026-08-19"), 0);
        assert_eq!(days_from_now("2026-08-17", "2026-08-19"), 1);
        assert_eq!(days_from_now("2026-08-09", "2026-08-19"), 9);
        // Not yet due: negative, i.e. days remaining.
        assert_eq!(days_from_now("2026-08-19", "2026-08-19"), -1);
        assert_eq!(days_from_now("2026-08-29", "2026-08-19"), -11);
        assert_eq!(days_from_now("1970-01-01", "2026-08-19"), 20683);
    }

    #[test]
    fn days_from_now_falls_back_to_zero_on_bad_input() {
        assert_eq!(days_from_now("garbage", "2026-08-19"), 0);
        assert_eq!(days_from_now("2026-08-19", "garbage"), 0);
        assert_eq!(days_from_now("", ""), 0);
    }

    #[test]
    fn meta_line_reads_the_day_count_per_section() {
        let today = "2026-08-19";
        assert_eq!(meta_line(Variant::Expired, Some("2026-08-17"), None, today), "1d overdue");
        assert_eq!(meta_line(Variant::Expired, Some("2026-08-18"), None, today), "overdue");
        assert_eq!(
            meta_line(Variant::Expired, Some("1970-01-01"), None, today),
            "20,683d overdue"
        );
        assert_eq!(meta_line(Variant::Upcoming, Some("2026-08-29"), None, today), "in 11d");
        assert_eq!(
            meta_line(Variant::Expired, Some("2026-08-17"), Some("[until 2026-08-17] gone"), today),
            "1d overdue · gone"
        );
        assert_eq!(meta_line(Variant::Undated, None, Some("use v2"), today), "use v2");
        assert_eq!(meta_line(Variant::Undated, None, None, today), "deprecated");
    }

    #[test]
    fn group_digits_inserts_thousands_separators() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(7), "7");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(20_683), "20,683");
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(group_digits(-1_234), "-1,234");
    }

    #[test]
    fn strip_until_drops_the_marker_and_the_space_after_it() {
        assert_eq!(strip_until("[until 2026-08-18] 더 이상 쓰이지 않음"), "더 이상 쓰이지 않음");
        assert_eq!(strip_until("[UNTIL 2026-01-02]  Use privateV2"), "Use privateV2");
        assert_eq!(strip_until("[  until   2026-01-02  ] gone"), "gone");
        assert_eq!(strip_until("use foo [until 2030-01-01] instead"), "use foo instead");
        assert_eq!(strip_until("[until 2026-08-18]"), "");
    }

    #[test]
    fn strip_until_leaves_a_reason_without_a_marker_alone() {
        assert_eq!(strip_until("Use privateV2 instead"), "Use privateV2 instead");
        assert_eq!(strip_until("  padded  "), "padded");
        assert_eq!(strip_until("[until 2026-1-2] kept"), "[until 2026-1-2] kept");
        assert_eq!(strip_until("[until2026-01-02] kept"), "[until2026-01-02] kept");
        assert_eq!(strip_until("[until 2026-01-02 kept"), "[until 2026-01-02 kept");
        assert_eq!(strip_until(""), "");
    }

    #[test]
    fn strip_until_takes_the_leftmost_marker() {
        assert_eq!(
            strip_until("[nope] and [until 2030-01-01] and [until 2040-01-01]"),
            "[nope] and and [until 2040-01-01]"
        );
    }
}
