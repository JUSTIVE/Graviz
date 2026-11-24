//! The Orphaned and Deprecated tabs: what the reachable slice leaves out.
//! Both list the FULL graph, since that is their whole point.

use std::collections::{HashMap, HashSet};

use graviz_core::graph::{NodeKind, default_root_ops, all_reachable_ids, is_until_expired, parse_until};
use nsh_core::prelude::*;
use nsh_metro::theme::{LIGHT, SEMIBOLD};

use crate::{
    model::{Model, today_string},
    store::Store,
    theme::{Color, Theme},
    ui::{c, metro, mono, th},
};

type S = crate::model::SharedString;

#[derive(Clone, PartialEq)]
enum OrphanRow {
    Kind { label: S, count: S },
    Type { card: u32, kind: NodeKind, name: S, fields: Option<S> },
}

#[derive(Clone, PartialEq)]
enum UntilRow {
    Banner { variant: Variant, count: S },
    Group { kind: NodeKind, name: S, count: S },
    Field { variant: Variant, card: u32, row: usize, name: S, date: Option<S>, meta: S },
}

struct Entry {
    card: u32,
    row: usize,
    name: S,
    type_name: S,
    kind: NodeKind,
    until: Option<String>,
    reason: Option<String>,
}

fn panel_badge(kind: NodeKind) -> impl IntoElement {
    view()
        .flex_none()
        .px(px(6.0))
        .bg(move || th().kind_color(kind).hsla())
        .child(text(kind.sdl_keyword()).text_size(px(9.0)).line_height(px(16.0)).font_weight(SEMIBOLD).text_color(rgb(0xffffff)))
}

/// A row click in either tab focuses that type on the canvas. Panels index
/// the full graph; the id is mapped into the current slice.
fn select(store: Store, node_index: usize, row: Option<usize>) {
    let full = store.full_model.get_untracked();
    let name = full.and_then(|m| m.graph.nodes.get(node_index).map(|n| n.id.clone()));
    let card = name.and_then(|n| store.model.with_untracked(|m| m.as_ref().and_then(|m| m.index_of.get(&n).copied())));
    if let Some(card) = card {
        store.navigate_to(card, row);
    }
}

pub fn orphans(store: Store) -> impl IntoElement {
    let selected = signal(None::<u32>);
    dynamic(move || {
        let Some(model) = store.full_model.get() else { return view().into_element() };
        selected.set(None);
        let (rows, total) = build_orphan_rows(&model);
        if total == 0 {
            return view()
                .flex_1()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .child(metro("no orphaned types found", 12.0).text_color(c(|t| t.text_muted)))
                .into_element();
        }
        let mut list = view();
        for row in rows {
            list = list.child(match row {
                OrphanRow::Kind { label, count } => view()
                    .w_full()
                    .h(px(28.0))
                    .flex_row()
                    .items_end()
                    .gap(px(4.0))
                    .px(px(12.0))
                    .pb(px(2.0))
                    .child(metro(label.to_string(), 10.0).font_weight(SEMIBOLD).text_color(c(|t| t.text_muted)))
                    .child(metro(count.to_string(), 10.0).font_weight(SEMIBOLD).opacity(0.6).text_color(c(|t| t.text_muted)))
                    .into_element(),
                OrphanRow::Type { card, kind, name, fields } => {
                    let is_sel = move || selected.get() == Some(card);
                    view()
                        .w_full()
                        .h(px(28.0))
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .px(px(12.0))
                        .border_l(px(2.0))
                        .border_color(move || if is_sel() { th().accent.hsla() } else { Color::CLEAR.hsla() })
                        .bg(move || if is_sel() { th().active_bg.hsla() } else { Color::CLEAR.hsla() })
                        .hover(|s| s.bg(th().hover_bg.hsla()))
                        .cursor(Cursor::Pointer)
                        .on_press(move || {
                            selected.set(Some(card));
                            select(store, card as usize, None);
                        })
                        .child(panel_badge(kind))
                        .child(mono(name.to_string(), 12.0).flex_1().min_w(px(0.)).truncate().text_color(c(|t| t.text)))
                        .child(when(fields.is_some(), move || {
                            metro(fields.clone().map(|f| f.to_string()).unwrap_or_default(), 10.0).flex_none().text_color(c(|t| t.text_faint))
                        }))
                        .into_element()
                }
            });
        }
        view()
            .flex_1()
            .min_h(px(0.))
            .child(
                metro(format!("{total} type{} unreachable from root", if total == 1 { "" } else { "s" }), 12.0)
                    .px(px(12.0))
                    .py(px(8.0))
                    .text_color(c(|t| t.text_muted)),
            )
            .child(scroll().flex_1().min_h(px(0.)).child(list))
            .into_element()
    })
}

pub fn deprecated(store: Store) -> impl IntoElement {
    let selected = signal(None::<u32>);
    dynamic(move || {
        let Some(model) = store.full_model.get() else { return view().into_element() };
        selected.set(None);
        let rows = build_until_rows(&model, &today_string());
        let mut list = view();
        for row in rows {
            list = list.child(match row {
                UntilRow::Banner { variant, count } => view()
                    .w_full()
                    .h(px(44.0))
                    .justify_center()
                    .child(
                        view()
                            .w_full()
                            .flex_row()
                            .items_start()
                            .gap(px(3.0))
                            .px(px(12.0))
                            .border_l(px(2.0))
                            .border_color(move || variant.date(&th()).hsla())
                            .child(metro(variant.title(), 20.0).line_height(px(26.0)).font_weight(LIGHT).text_color(move || variant.text(&th()).hsla()))
                            .child(
                                metro(count.to_string(), 10.0)
                                    .mt(px(2.0))
                                    .line_height(px(12.0))
                                    .font_weight(SEMIBOLD)
                                    .opacity(0.8)
                                    .text_color(move || variant.text(&th()).hsla()),
                            ),
                    )
                    .into_element(),
                UntilRow::Group { kind, name, count } => view()
                    .w_full()
                    .h(px(44.0))
                    .justify_center()
                    .child(
                        view()
                            .w_full()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(12.0))
                            .py(px(4.0))
                            .child(panel_badge(kind))
                            .child(mono(name.to_string(), 10.0).flex_1().min_w(px(0.)).truncate().text_color(c(|t| t.text_muted)))
                            .child(metro(count.to_string(), 10.0).flex_none().opacity(0.6).text_color(c(|t| t.text))),
                    )
                    .into_element(),
                UntilRow::Field { variant, card, row, name, date, meta } => {
                    let is_sel = move || selected.get() == Some(card);
                    view()
                        .w_full()
                        .h(px(44.0))
                        .justify_center()
                        .gap(px(2.0))
                        .px(px(12.0))
                        .bg(move || if is_sel() { variant.focus(&th()).hsla() } else { Color::CLEAR.hsla() })
                        .hover(|s| s.bg(th().hover_bg.hsla()))
                        .cursor(Cursor::Pointer)
                        .on_press(move || {
                            selected.set(Some(card));
                            select(store, card as usize, Some(row));
                        })
                        .child(
                            view()
                                .w_full()
                                .flex_row()
                                .items_center()
                                .gap(px(8.0))
                                .child(
                                    mono(name.to_string(), 12.0)
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .line_through()
                                        .text_color(move || variant.text(&th()).hsla()),
                                )
                                .child(when(date.is_some(), move || {
                                    mono(date.clone().map(|d| d.to_string()).unwrap_or_default(), 10.0)
                                        .flex_none()
                                        .text_color(move || variant.date(&th()).hsla())
                                })),
                        )
                        .child(metro(meta.to_string(), 10.0).w_full().truncate().text_color(c(|t| t.text_muted)))
                        .into_element()
                }
            });
        }
        scroll().flex_1().min_h(px(0.)).child(list).into_element()
    })
}

const KIND_ORDER: [NodeKind; 6] = [
    NodeKind::Object,
    NodeKind::Interface,
    NodeKind::Union,
    NodeKind::Enum,
    NodeKind::Input,
    NodeKind::Scalar,
];

/// Row pitch of the orphan list (the web's `py-1.5` type button).

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
            label: S::from(kind_upper(kind)),
            count: S::from(format!("({})", list.len())),
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
                    .map(|f| S::from(format!("{}f", f.len()))),
            });
        }
    }
    (rows, total)
}


#[derive(Clone, Copy, PartialEq, Eq)]
enum Variant {
    Expired,
    Upcoming,
    Undated,
}

impl Variant {
    /// Section title, a lowercase Metro header.
    fn title(self) -> &'static str {
        match self {
            Variant::Expired => "expired",
            Variant::Upcoming => "upcoming",
            Variant::Undated => "deprecated",
        }
    }

    /// Banner text and field-name color.
    fn text(self, th: &Theme) -> Color {
        match self {
            Variant::Expired => th.red,
            Variant::Upcoming => th.type_amber,
            Variant::Undated => th.text_muted,
        }
    }

    /// The 10%-tinted banner background, reused as the selected-row wash.
    fn tint(self, th: &Theme) -> Color {
        match self {
            Variant::Expired => th.expired.opacity(0.1),
            Variant::Upcoming => th.type_amber.opacity(0.1),
            Variant::Undated => th.text_muted.opacity(0.1),
        }
    }

    /// Selected-row background (the web's `focus` class).
    fn focus(self, th: &Theme) -> Color {
        match self {
            Variant::Undated => th.active_bg,
            _ => self.tint(th),
        }
    }

    /// Color of the trailing `YYYY-MM-DD` chip.
    fn date(self, th: &Theme) -> Color {
        match self {
            Variant::Expired => th.expired,
            _ => th.type_amber,
        }
    }
}


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
                name: S::from(name.to_string()),
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
        count: S::from(entries.len().to_string()),
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
            count: S::from(format!("({})", group.len())),
        });
        for entry in group {
            let meta = meta_line(variant, entry.until.as_deref(), entry.reason.as_deref(), today);
            rows.push(UntilRow::Field {
                variant,
                card: entry.card,
                row: entry.row,
                name: entry.name,
                date: entry.until.map(S::from),
                meta: S::from(meta),
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
