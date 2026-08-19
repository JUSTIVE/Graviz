//! View-model: node cards with geometry shared by painting and hit-testing.
//!
//! In the web app the card layout math existed three times (drawNodeSprite,
//! trailingSectionGeom, hitTestField) and drifted. Here `Card::row_y` /
//! `Card::row_at` are the single source of truth.

use gompass_core::graph::{
    all_reachable_ids, default_root_ops, is_until_expired, reachable_from, EdgeKind, NodeKind,
    ParsedGraph,
};
use gompass_core::layout::{self, LayoutConfig, LayoutEdge, LayoutNode};
use std::collections::{HashMap, HashSet};

// Card geometry — mirrors the web renderer's node-style.ts constants exactly
// (drawNodeSprite / estimateNodeWidth / estimateNodeHeight / trailingSectionGeom).
pub const HEADER_H: f32 = 42.0;
pub const HEADER_H_WITH_DESC: f32 = 56.0;
pub const ROW_H: f32 = 14.0;
/// Row pitch when field descriptions are shown inline.
pub const ROW_H_WITH_DESC: f32 = 26.0;
/// Trailing (implements / member-of-union) rows always use this pitch.
pub const TIGHT_ROW_H: f32 = 14.0;
pub const TOP_BODY_PAD: f32 = 8.0;
pub const BOTTOM_PAD: f32 = 10.0;
pub const IMPL_SECTION_GAP: f32 = 8.0;
/// Left inset of body-row text (the 10px gutter holds the overlay marker).
pub const CARD_PAD_X: f32 = 10.0;
/// Header text inset.
pub const HEADER_PAD_X: f32 = 8.0;
pub const NAME_FONT_PX: f32 = 13.0;
pub const ROW_FONT_PX: f32 = 10.0;
pub const DESC_FONT_PX: f32 = 9.0;
pub const KIND_FONT_PX: f32 = 9.0;
pub const BAND_FONT_PX: f32 = 10.0;
/// Menlo advance width / font size.
pub const MONO_ADVANCE: f32 = 0.602;
pub const MIN_CARD_W: f32 = 220.0;
pub const MAX_CARD_W: f32 = 900.0;
const NAME_H_PAD: f32 = 16.0;
const FIELD_NAME_TYPE_GAP: f32 = 16.0;
const FIELD_ROW_SIDE_PAD: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Field,
    EnumValue,
    /// Member of a `union` type — always tight pitch, sky-colored.
    UnionMember,
}

/// Which palette entry the right-aligned return type uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeColor {
    Normal,
    BuiltinScalar,
    Expired,
}

/// A hit inside a card's body, in the three vertical zones the web renderer
/// paints: body rows, the implements band, the member-of-union band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowHit {
    Row(usize),
    Implements(usize),
    MemberOfUnion(usize),
}

/// Vertical geometry of the two trailing wash bands (`trailingSectionGeom`).
#[derive(Debug, Clone, Copy, Default)]
pub struct BandGeom {
    pub iface_band_top: f32,
    pub iface_band_bottom: f32,
    pub iface_rows_top: f32,
    pub union_band_top: f32,
    pub union_rows_top: f32,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub left: gpui::SharedString,
    /// Right-aligned type expression (empty for enum values / sections).
    pub right: gpui::SharedString,
    /// Card index this row navigates to on click, if any.
    pub target: Option<u32>,
    pub deprecated: bool,
    pub is_overlay: bool,
    pub description: Option<String>,
    /// Single-line description, pre-truncated to the card width, for
    /// show-descriptions mode.
    pub description_line: Option<gpui::SharedString>,
    pub deprecation_reason: Option<String>,
    /// The `[until YYYY-MM-DD]` sunset date has passed.
    pub until_expired: bool,
    /// Field type was unwrapped through a Relay Connection.
    pub is_relay: bool,
    pub type_color: TypeColor,
    /// Right-hand type, pre-fitted to the card width (the painter must not
    /// re-run `fit_text` every frame).
    pub right_fit: gpui::SharedString,
    /// Width of `right_fit`, for right-alignment and the hover chip.
    pub right_w: f32,
    /// Field args, for the sidebar's arity badge and hover list.
    pub args: Vec<(gpui::SharedString, gpui::SharedString)>,
    pub required_args: usize,
}

#[derive(Debug, Clone)]
pub struct Card {
    /// Index into `ParsedGraph::nodes` / `Model::cards`.
    pub index: u32,
    pub name: gpui::SharedString,
    /// Header name pre-fitted to the card width.
    pub name_fit: gpui::SharedString,
    pub kind: NodeKind,
    /// Lowercase word for sidebar badges ("type", "interface", …).
    pub kind_label: &'static str,
    /// Uppercase word the card header paints ("OBJECT", "INTERFACE", …).
    pub kind_upper: &'static str,
    pub description: Option<String>,
    /// One-line header description, shown in show-descriptions mode.
    pub header_desc: Option<gpui::SharedString>,
    pub rows: Vec<Row>,
    /// Trailing violet band (Object / Interface), pre-fitted.
    pub implements: Vec<gpui::SharedString>,
    /// Trailing amber band (Object), pre-fitted.
    pub member_of_unions: Vec<gpui::SharedString>,
    pub w: f32,
    pub h: f32,
    /// Row pitch — `ROW_H`, or `ROW_H_WITH_DESC` in show-descriptions mode.
    /// Union members and trailing rows always use `TIGHT_ROW_H`.
    pub row_h: f32,
    pub header_h: f32,
    pub band: BandGeom,
    /// Whole type came from the overlay SDL.
    pub is_overlay: bool,
    /// Graph row (field idx, then enum-value idx) → display row, `None` when
    /// hidden by the primitive-fields filter. Ports, pins and search hits go
    /// through this map.
    pub row_map: Vec<Option<u32>>,
}

impl Card {
    /// Top of the body row grid: `headerH + TOP_BODY_PAD - 2` in the web.
    pub fn body_top(&self) -> f32 {
        self.header_h + TOP_BODY_PAD - 2.0
    }

    /// Pitch of the body rows — union members are always tight.
    pub fn body_pitch(&self) -> f32 {
        if self.kind == NodeKind::Union {
            TIGHT_ROW_H
        } else {
            self.row_h
        }
    }

    pub fn row_y(&self, i: usize) -> f32 {
        self.body_top() + i as f32 * self.body_pitch()
    }

    /// Text baseline for body row `i` (`bodyY + i*rowH + 10`).
    pub fn row_baseline(&self, i: usize) -> f32 {
        self.row_y(i) + 10.0
    }

    /// Inverse of the painter's geometry, across all three body zones.
    pub fn hit_row(&self, local_x: f32, local_y: f32) -> Option<RowHit> {
        if local_x < 0.0 || local_x > self.w || local_y < self.body_top() {
            return None;
        }
        // trailing bands first — they sit below the body rows
        if !self.member_of_unions.is_empty() && local_y >= self.band.union_rows_top {
            let k = ((local_y - self.band.union_rows_top) / TIGHT_ROW_H) as usize;
            if k < self.member_of_unions.len() {
                return Some(RowHit::MemberOfUnion(k));
            }
        }
        if !self.implements.is_empty() && local_y >= self.band.iface_rows_top {
            let k = ((local_y - self.band.iface_rows_top) / TIGHT_ROW_H) as usize;
            if k < self.implements.len() {
                return Some(RowHit::Implements(k));
            }
        }
        let i = ((local_y - self.body_top()) / self.body_pitch()) as usize;
        (i < self.rows.len()).then_some(RowHit::Row(i))
    }

    /// Display row for a graph-space row index (field, then enum value).
    pub fn display_row(&self, graph_row: usize) -> Option<usize> {
        self.row_map.get(graph_row).copied().flatten().map(|i| i as usize)
    }

    pub fn port_y(&self, field_index: Option<usize>) -> f32 {
        match field_index.and_then(|i| self.display_row(i)) {
            Some(i) if i < self.rows.len() => self.row_y(i) + self.body_pitch() / 2.0,
            _ => self.h / 2.0,
        }
    }
}

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

/// Longest prefix of `s` that fits `max_w` at `font_px`, with an ellipsis
/// when clipped (the web's `fitText`).
pub fn fit_text(s: &str, font_px: f32, max_w: f32) -> String {
    if mono_w(s, font_px) <= max_w {
        return s.to_string();
    }
    let ell = mono_w("…", font_px);
    let budget = (max_w - ell).max(0.0);
    let per = font_px * MONO_ADVANCE;
    let n = (budget / per).floor() as usize;
    if n == 0 {
        return "…".to_string();
    }
    let cut: String = s.chars().take(n).collect();
    format!("{cut}…")
}

/// Whitespace-collapsed single line.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `trailingSectionGeom` — vertical geometry of the two wash bands.
fn band_geom(
    kind: NodeKind,
    h: f32,
    header_h: f32,
    row_h: f32,
    fields: usize,
    ifaces: usize,
    unions: usize,
) -> BandGeom {
    if !matches!(kind, NodeKind::Object | NodeKind::Interface) || (ifaces == 0 && unions == 0) {
        return BandGeom::default();
    }
    let impl_gap = if ifaces > 0 && fields > 0 { IMPL_SECTION_GAP } else { 0.0 };
    let union_gap = if unions > 0 && ifaces == 0 && fields > 0 { IMPL_SECTION_GAP } else { 0.0 };
    let wash_top = header_h + TOP_BODY_PAD + fields as f32 * row_h - 2.0 + impl_gap + union_gap;
    let iface_block = ifaces as f32 * TIGHT_ROW_H;
    let union_block = unions as f32 * TIGHT_ROW_H;
    let extra = (h - wash_top - iface_block - union_block).max(0.0);

    let iface_band_top = wash_top;
    let iface_band_bottom = if ifaces > 0 {
        if unions > 0 {
            wash_top + iface_block + (extra / 2.0).floor()
        } else {
            h
        }
    } else {
        wash_top
    };
    let union_band_top = if ifaces > 0 { iface_band_bottom } else { wash_top };
    BandGeom {
        iface_band_top,
        iface_band_bottom,
        iface_rows_top: iface_band_top
            + ((iface_band_bottom - iface_band_top - iface_block) / 2.0).max(0.0).floor(),
        union_band_top,
        union_rows_top: union_band_top
            + ((h - union_band_top - union_block) / 2.0).max(0.0).floor(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeGroup {
    FieldNonNull,
    FieldNullable,
    Union,
    Implements,
    Arg,
}

#[derive(Debug, Clone)]
pub struct EdgeVisual {
    pub from: u32,
    pub to: u32,
    pub group: EdgeGroup,
    /// World-space start point of the curve.
    pub start: layout::Point,
    /// Cubic segments, drawn as real Béziers (no polyline approximation).
    pub curves: Vec<layout::CubicSeg>,
    /// Coarse flattening of the same curve, for hit-testing and culling.
    pub points: Vec<f32>,
    /// World-space bbox (min_x, min_y, max_x, max_y) for culling.
    pub bbox: [f32; 4],
    /// Number of parallel field edges collapsed into this one (≥1).
    pub bundled: u32,
    /// Incident to a hub node (degree ≥ HUB_FADE_DEGREE) — drawn faded so
    /// Relay `Node`-style stars don't dominate the picture.
    pub hub_faded: bool,
    /// Field names (or relation kind) this edge represents — several when
    /// bundled.
    pub labels: Vec<gpui::SharedString>,
}

/// In- or out-degree at which a node's edges get faded.
pub const HUB_FADE_DEGREE: u32 = 50;

/// Knobs on [`build_model`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOptions {
    /// Draw a description line under each field row (taller cards).
    pub show_descriptions: bool,
    /// Collapse parallel field edges sharing source→target into one arrow.
    pub bundle_edges: bool,
    /// Hide fields whose return type is a builtin scalar (String/Int/…).
    pub hide_primitive_fields: bool,
    /// Today as `YYYY-MM-DD`, for `[until]` expiry coloring.
    pub today: String,
}

impl Default for ModelOptions {
    fn default() -> Self {
        ModelOptions {
            show_descriptions: false,
            bundle_edges: true,
            hide_primitive_fields: false,
            today: today_string(),
        }
    }
}

const BUILTIN_SCALARS: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];

/// Today's civil date (UTC) as `YYYY-MM-DD` without pulling in chrono.
pub fn today_string() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Howard Hinnant's civil_from_days
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

pub struct Model {
    pub graph: ParsedGraph,
    pub cards: Vec<Card>,
    /// Top-left world position per card.
    pub positions: Vec<layout::Point>,
    pub edges: Vec<EdgeVisual>,
    pub world_w: f32,
    pub world_h: f32,
    pub index_of: HashMap<String, u32>,
    pub schema_name: String,
    /// Count of overlay-marked cards + rows (0 when no overlay is applied).
    pub overlay_marks: usize,
    /// Card indices of the root operation types present in this slice.
    pub roots: Vec<u32>,
    #[allow(dead_code)]
    pub options: ModelOptions,
    /// (documented, total) over types + field/enum rows.
    pub desc_coverage: (usize, usize),
}

/// The three canvas modes of the web app's `/view` tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Types reachable from the effective root (default view).
    Reachable,
    /// Types no root operation can reach.
    Orphaned,
    /// Types owning deprecated members, plus the targets of those members.
    Deprecated,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Reachable => "Reachable",
            Mode::Orphaned => "Orphaned",
            Mode::Deprecated => "Deprecated",
        }
    }
}

fn root_ops_of(graph: &ParsedGraph) -> HashSet<String> {
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


/// Cut the full graph down to the slice a mode shows.
pub fn slice_graph(full: &ParsedGraph, mode: Mode, root_override: Option<&str>) -> ParsedGraph {
    let root_ops = root_ops_of(full);
    let (nodes, edges): (Vec<_>, Vec<_>) = match mode {
        Mode::Reachable => {
            let root = root_override
                .map(str::to_string)
                .or_else(|| full.root_types.query.clone())
                .or_else(|| full.root_types.mutation.clone())
                .or_else(|| full.root_types.subscription.clone());
            let Some(root) = root else {
                return full.clone();
            };
            let sub = reachable_from(&full.nodes, &full.edges, &root, &root_ops);
            (
                sub.nodes.into_iter().cloned().collect(),
                sub.edges.into_iter().cloned().collect(),
            )
        }
        Mode::Orphaned => {
            let reach = all_reachable_ids(&full.nodes, &full.edges, &root_ops);
            let nodes: Vec<_> = full
                .nodes
                .iter()
                .filter(|n| !reach.contains(&n.id))
                .cloned()
                .collect();
            let keep: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
            let edges = full
                .edges
                .iter()
                .filter(|e| keep.contains(e.source.as_str()) && keep.contains(e.target.as_str()))
                .cloned()
                .collect();
            (nodes, edges)
        }
        Mode::Deprecated => {
            let mut keep: HashSet<String> = HashSet::new();
            for n in &full.nodes {
                let fields = n.fields.as_deref().unwrap_or(&[]);
                let has_deprecated = fields.iter().any(|f| f.is_deprecated)
                    || n.values.as_deref().unwrap_or(&[]).iter().any(|v| v.is_deprecated);
                if has_deprecated {
                    keep.insert(n.id.clone());
                    for f in fields.iter().filter(|f| f.is_deprecated) {
                        keep.insert(f.type_name.clone());
                    }
                }
            }
            let nodes: Vec<_> = full
                .nodes
                .iter()
                .filter(|n| keep.contains(&n.id))
                .cloned()
                .collect();
            let by_id: HashMap<&str, &gompass_core::graph::GraphNodeData> =
                full.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
            let edges = full
                .edges
                .iter()
                .filter(|e| {
                    if !keep.contains(&e.source) || !keep.contains(&e.target) {
                        return false;
                    }
                    if e.kind != EdgeKind::Field {
                        return true;
                    }
                    // keep only edges leaving a deprecated field
                    e.source_field_index
                        .and_then(|i| by_id.get(e.source.as_str())?.fields.as_deref()?.get(i))
                        .is_some_and(|f| f.is_deprecated)
                })
                .cloned()
                .collect();
            (nodes, edges)
        }
    };
    ParsedGraph {
        nodes,
        edges,
        error: None,
        warnings: Vec::new(),
        root_types: full.root_types.clone(),
    }
}

pub fn mono_w(text: &str, font_px: f32) -> f32 {
    text.chars().count() as f32 * font_px * MONO_ADVANCE
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Object => "type",
        NodeKind::Interface => "interface",
        NodeKind::Union => "union",
        NodeKind::Enum => "enum",
        NodeKind::Scalar => "scalar",
        NodeKind::Input => "input",
    }
}

pub fn build_model(graph: ParsedGraph, schema_name: String, options: &ModelOptions) -> Model {
    let mut index_of: HashMap<String, u32> = HashMap::with_capacity(graph.nodes.len());
    for (i, n) in graph.nodes.iter().enumerate() {
        index_of.insert(n.id.clone(), i as u32);
    }

    // ---- cards ----
    let mut cards: Vec<Card> = Vec::with_capacity(graph.nodes.len());
    for (i, n) in graph.nodes.iter().enumerate() {
        let base_row = |kind: RowKind, left: gpui::SharedString, target: Option<u32>| Row {
            kind,
            left,
            right: gpui::SharedString::default(),
            target,
            deprecated: false,
            is_overlay: false,
            description: None,
            description_line: None,
            deprecation_reason: None,
            until_expired: false,
            is_relay: false,
            type_color: TypeColor::Normal,
            right_fit: gpui::SharedString::default(),
            right_w: 0.0,
            args: Vec::new(),
            required_args: 0,
        };
        // Body rows: fields, OR enum values, OR union members — never mixed
        // (the web renderer paints exactly one of these grids).
        let mut rows: Vec<Row> = Vec::new();
        let mut row_map: Vec<Option<u32>> = Vec::new();
        for f in n.fields.as_deref().unwrap_or(&[]) {
            if options.hide_primitive_fields && BUILTIN_SCALARS.contains(&f.type_name.as_str()) {
                row_map.push(None);
                continue;
            }
            row_map.push(Some(rows.len() as u32));
            let mut r = base_row(
                RowKind::Field,
                f.name.clone().into(),
                index_of.get(&f.type_name).copied(),
            );
            r.right = f.type_.clone().into();
            r.deprecated = f.is_deprecated;
            r.is_overlay = f.is_overlay;
            r.description = f.description.clone();
            r.deprecation_reason = f.deprecation_reason.clone();
            r.until_expired = is_until_expired(f.until.as_deref(), &options.today);
            r.is_relay = f.is_relay_connection;
            r.type_color = if r.until_expired {
                TypeColor::Expired
            } else if BUILTIN_SCALARS.contains(&f.type_name.as_str()) {
                TypeColor::BuiltinScalar
            } else {
                TypeColor::Normal
            };
            if let Some(args) = &f.args {
                r.required_args = args.iter().filter(|a| a.type_.ends_with('!')).count();
                r.args = args
                    .iter()
                    .map(|a| (a.name.clone().into(), a.type_.clone().into()))
                    .collect();
            }
            rows.push(r);
        }
        for v in n.values.as_deref().unwrap_or(&[]) {
            row_map.push(Some(rows.len() as u32));
            let mut r = base_row(RowKind::EnumValue, v.name.clone().into(), None);
            r.deprecated = v.is_deprecated;
            r.is_overlay = v.is_overlay;
            r.description = v.description.clone();
            r.deprecation_reason = v.deprecation_reason.clone();
            r.until_expired = is_until_expired(v.until.as_deref(), &options.today);
            rows.push(r);
        }
        for m in n.members.as_deref().unwrap_or(&[]) {
            rows.push(base_row(
                RowKind::UnionMember,
                m.clone().into(),
                index_of.get(m).copied(),
            ));
        }
        // Trailing wash bands, not body rows.
        let implements: Vec<gpui::SharedString> = n
            .interfaces
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|s| s.clone().into())
            .collect();
        let member_of_unions: Vec<gpui::SharedString> = n
            .member_of_unions
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|s| s.clone().into())
            .collect();

        // ---- width: estimateNodeWidth ----
        let text_w = mono_w(&n.name, NAME_FONT_PX);
        let mut field_max = 0.0f32;
        {
            let mut consider = |name: &str, ty: &str, relay: bool| {
                let relay_pad = if relay { mono_w("~~ ", ROW_FONT_PX) } else { 0.0 };
                let w = mono_w(name, ROW_FONT_PX)
                    + FIELD_NAME_TYPE_GAP
                    + relay_pad
                    + mono_w(ty, ROW_FONT_PX)
                    + FIELD_ROW_SIDE_PAD;
                field_max = field_max.max(w);
            };
            for r in &rows {
                consider(&r.left, &r.right, r.is_relay);
            }
            for s in implements.iter().chain(member_of_unions.iter()) {
                consider(s, "", false);
            }
        }
        let required = NAME_H_PAD + text_w.ceil();
        let w = MIN_CARD_W.max(MAX_CARD_W.min(required)).max(field_max).round();

        // ---- height: estimateNodeHeight ----
        let row_h = if options.show_descriptions { ROW_H_WITH_DESC } else { ROW_H };
        let header_h = if options.show_descriptions { HEADER_H_WITH_DESC } else { HEADER_H };
        let field_count = if n.kind == NodeKind::Union { 0 } else { rows.len() };
        let (grid_rows, tight_rows) = match n.kind {
            NodeKind::Union => (0, rows.len()),
            NodeKind::Scalar => (0, 0),
            _ => (rows.len(), implements.len() + member_of_unions.len()),
        };
        let body = if grid_rows == 0 && tight_rows == 0 {
            row_h
        } else {
            grid_rows as f32 * row_h + tight_rows as f32 * TIGHT_ROW_H
        };
        let impl_gap = if matches!(n.kind, NodeKind::Object | NodeKind::Interface)
            && !implements.is_empty()
            && field_count > 0
        {
            IMPL_SECTION_GAP
        } else {
            0.0
        };
        let union_gap = if n.kind == NodeKind::Object
            && !member_of_unions.is_empty()
            && implements.is_empty()
            && field_count > 0
        {
            IMPL_SECTION_GAP
        } else {
            0.0
        };
        let h = header_h + TOP_BODY_PAD + body + impl_gap + union_gap + BOTTOM_PAD;

        let band = band_geom(
            n.kind,
            h,
            header_h,
            row_h,
            field_count,
            implements.len(),
            member_of_unions.len(),
        );

        // Everything the painter would otherwise re-fit every frame.
        for r in &mut rows {
            if r.right.is_empty() {
                continue;
            }
            let name_w = mono_w(&r.left, ROW_FONT_PX);
            let relay_pad = if r.is_relay { 20.0 } else { 0.0 };
            let max_w = (w - 20.0 - name_w - relay_pad - 8.0).max(40.0);
            let fitted = fit_text(&r.right, ROW_FONT_PX, max_w);
            r.right_w = mono_w(&fitted, ROW_FONT_PX);
            r.right_fit = fitted.into();
        }
        let name_fit: gpui::SharedString =
            fit_text(&n.name, NAME_FONT_PX, w - HEADER_PAD_X * 2.0).into();
        let implements: Vec<gpui::SharedString> = implements
            .iter()
            .map(|s| gpui::SharedString::from(fit_text(s, BAND_FONT_PX, w - 20.0)))
            .collect();
        let member_of_unions: Vec<gpui::SharedString> = member_of_unions
            .iter()
            .map(|s| gpui::SharedString::from(fit_text(s, BAND_FONT_PX, w - 20.0)))
            .collect();

        // Descriptions are pre-fitted to the card width, like the painter.
        let header_desc = if options.show_descriptions {
            n.description
                .as_deref()
                .map(|d| one_line(d))
                .filter(|d| !d.is_empty())
                .map(|d| fit_text(&d, DESC_FONT_PX, w - HEADER_PAD_X * 2.0).into())
        } else {
            None
        };
        if options.show_descriptions {
            for r in &mut rows {
                let src = r
                    .description
                    .clone()
                    .or_else(|| r.deprecated.then(|| r.deprecation_reason.clone()).flatten());
                if let Some(d) = src {
                    let d = one_line(&d);
                    if !d.is_empty() {
                        r.description_line =
                            Some(fit_text(&d, DESC_FONT_PX, w - CARD_PAD_X * 2.0).into());
                    }
                }
            }
        }

        cards.push(Card {
            index: i as u32,
            name: n.name.clone().into(),
            name_fit,
            kind: n.kind,
            kind_label: kind_label(n.kind),
            kind_upper: kind_upper(n.kind),
            description: n.description.clone(),
            header_desc,
            rows,
            implements,
            member_of_unions,
            w,
            h,
            row_h,
            header_h,
            band,
            is_overlay: n.is_overlay,
            row_map,
        });
    }
    let overlay_marks = cards
        .iter()
        .map(|c| c.is_overlay as usize + c.rows.iter().filter(|r| r.is_overlay).count())
        .sum();
    // Description coverage over types + field/enum rows (Investigate mode).
    let mut desc_total = 0usize;
    let mut desc_documented = 0usize;
    for c in &cards {
        desc_total += 1;
        desc_documented += c.description.is_some() as usize;
        for r in &c.rows {
            if matches!(r.kind, RowKind::Field | RowKind::EnumValue) {
                desc_total += 1;
                desc_documented += r.description.is_some() as usize;
            }
        }
    }

    // ---- layout ----
    let layout_nodes: Vec<LayoutNode> =
        cards.iter().map(|c| LayoutNode { w: c.w, h: c.h }).collect();
    // Two passes: endpoints first (for hub degrees), then the layout edges
    // with lane-routing turned off for hub-incident edges.
    struct PreEdge {
        from: u32,
        to: u32,
        edge_index: usize,
        bundled: u32,
        labels: Vec<gpui::SharedString>,
    }
    let mut pre: Vec<PreEdge> = Vec::new();
    let mut bundle_index: HashMap<(u32, u32), usize> = HashMap::new();
    for (ei, e) in graph.edges.iter().enumerate() {
        let (Some(&from), Some(&to)) = (index_of.get(&e.source), index_of.get(&e.target)) else {
            continue;
        };
        if from == to {
            continue;
        }
        let label: gpui::SharedString = match e.kind {
            EdgeKind::Field | EdgeKind::Arg => {
                e.source_field.clone().unwrap_or_default().into()
            }
            EdgeKind::Implements => "implements".into(),
            EdgeKind::Union => "union member".into(),
        };
        if options.bundle_edges && e.kind == EdgeKind::Field {
            if let Some(&idx) = bundle_index.get(&(from, to)) {
                pre[idx].bundled += 1;
                pre[idx].labels.push(label);
                continue;
            }
            bundle_index.insert((from, to), pre.len());
        }
        pre.push(PreEdge { from, to, edge_index: ei, bundled: 1, labels: vec![label] });
    }
    let mut degree = vec![0u32; cards.len()];
    for p in &pre {
        degree[p.from as usize] += 1;
        degree[p.to as usize] += 1;
    }
    let mut layout_edges: Vec<LayoutEdge> = Vec::with_capacity(pre.len());
    let mut kept_edges: Vec<usize> = Vec::with_capacity(pre.len());
    let mut bundle_counts: Vec<u32> = Vec::with_capacity(pre.len());
    for p in &pre {
        let e = &graph.edges[p.edge_index];
        layout_edges.push(LayoutEdge {
            from: p.from,
            to: p.to,
            from_port_y: cards[p.from as usize].port_y(e.source_field_index),
            route: degree[p.from as usize] < HUB_FADE_DEGREE
                && degree[p.to as usize] < HUB_FADE_DEGREE,
        });
        kept_edges.push(p.edge_index);
        bundle_counts.push(p.bundled);
    }
    let rt = &graph.root_types;
    let roots: Vec<u32> = [&rt.query, &rt.mutation, &rt.subscription]
        .into_iter()
        .filter_map(|name| name.as_ref().and_then(|n| index_of.get(n).copied()))
        .collect();
    let result = layout::layout(&layout_nodes, &layout_edges, &roots, &LayoutConfig::default());

    // ---- flatten edge paths ----
    let mut edges: Vec<EdgeVisual> = Vec::with_capacity(result.edges.len());
    for path in &result.edges {
        let le = &layout_edges[path.edge_index as usize];
        let ge = &graph.edges[kept_edges[path.edge_index as usize]];
        let group = edge_group(ge);
        let mut points = Vec::with_capacity(path.points.len() * 2);
        let mut bbox = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for p in &path.points {
            points.push(p.x);
            points.push(p.y);
            bbox[0] = bbox[0].min(p.x);
            bbox[1] = bbox[1].min(p.y);
            bbox[2] = bbox[2].max(p.x);
            bbox[3] = bbox[3].max(p.y);
        }
        edges.push(EdgeVisual {
            from: le.from,
            to: le.to,
            group,
            start: path.start,
            curves: path.curves.clone(),
            points,
            bbox,
            bundled: bundle_counts[path.edge_index as usize],
            hub_faded: degree[le.from as usize] >= HUB_FADE_DEGREE
                || degree[le.to as usize] >= HUB_FADE_DEGREE,
            labels: pre[path.edge_index as usize].labels.clone(),
        });
    }

    Model {
        cards,
        positions: result.positions,
        edges,
        world_w: result.width,
        world_h: result.height,
        index_of,
        schema_name,
        overlay_marks,
        roots,
        options: options.clone(),
        desc_coverage: (desc_documented, desc_total),
        graph,
    }
}

fn edge_group(e: &gompass_core::graph::GraphEdgeData) -> EdgeGroup {
    use gompass_core::graph::EdgeKind;
    match e.kind {
        EdgeKind::Field => {
            if e.nullable.unwrap_or(false) {
                EdgeGroup::FieldNullable
            } else {
                EdgeGroup::FieldNonNull
            }
        }
        EdgeKind::Union => EdgeGroup::Union,
        EdgeKind::Implements => EdgeGroup::Implements,
        EdgeKind::Arg => EdgeGroup::Arg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gompass_core::graph::{sdl_to_graph, SdlToGraphOptions};

    fn model_of(sdl: &str, opts: ModelOptions) -> Model {
        let g = sdl_to_graph(
            sdl,
            &SdlToGraphOptions { hide_relay_boilerplate: false, ..Default::default() },
        );
        assert!(g.error.is_none(), "{:?}", g.error);
        build_model(g, "t".into(), &opts)
    }

    /// The spec's worked example: Object with 3 fields + 1 interface, no
    /// descriptions → h=124, band 98..124, iface rows start at 104 so the
    /// band text baseline lands at 114.
    #[test]
    fn card_height_and_band_match_the_web_worked_example() {
        let m = model_of(
            "interface Node { id: ID! }
             type User implements Node { id: ID!, a: String, b: String }",
            ModelOptions { today: "2020-01-01".into(), ..Default::default() },
        );
        let c = &m.cards[m.index_of["User"] as usize];
        assert_eq!(c.rows.len(), 3);
        assert_eq!(c.implements.len(), 1);
        assert_eq!(c.h, 124.0, "card height");
        assert_eq!(c.band.iface_band_top, 98.0, "wash top");
        assert_eq!(c.band.iface_band_bottom, 124.0, "band runs to the card bottom");
        assert_eq!(c.band.iface_rows_top, 104.0, "rows centered in the band");
        assert_eq!(c.band.iface_rows_top + 10.0, 114.0, "band text baseline");
    }

    #[test]
    fn body_row_grid_matches_the_web() {
        let m = model_of(
            "type Query { a: String, b: String }",
            ModelOptions { today: "2020-01-01".into(), ..Default::default() },
        );
        let c = &m.cards[m.index_of["Query"] as usize];
        assert_eq!(c.header_h, HEADER_H);
        assert_eq!(c.body_top(), HEADER_H + 6.0, "bodyY = headerH + TOP_BODY_PAD - 2");
        assert_eq!(c.row_baseline(0), HEADER_H + 16.0, "first baseline");
        assert_eq!(c.row_baseline(1) - c.row_baseline(0), ROW_H, "pitch");
        // hit-testing is the exact inverse of the painter
        assert_eq!(c.hit_row(20.0, c.row_y(1) + 1.0), Some(RowHit::Row(1)));
        assert_eq!(c.hit_row(20.0, 10.0), None, "header is not a row");
    }

    #[test]
    fn descriptions_mode_uses_the_taller_grid() {
        let m = model_of(
            "\"doc\" type Query { a: String }",
            ModelOptions {
                show_descriptions: true,
                today: "2020-01-01".into(),
                ..Default::default()
            },
        );
        let c = &m.cards[m.index_of["Query"] as usize];
        assert_eq!(c.header_h, HEADER_H_WITH_DESC);
        assert_eq!(c.row_h, ROW_H_WITH_DESC);
        assert!(c.header_desc.is_some(), "header description is rendered");
    }

    #[test]
    fn union_members_use_tight_pitch_and_scalars_get_a_stub_row() {
        let m = model_of(
            "type A { x: String } type B { x: String } union U = A | B scalar Custom",
            ModelOptions {
                show_descriptions: true,
                today: "2020-01-01".into(),
                ..Default::default()
            },
        );
        let u = &m.cards[m.index_of["U"] as usize];
        assert_eq!(u.body_pitch(), TIGHT_ROW_H, "union members stay tight");
        assert_eq!(u.h, HEADER_H_WITH_DESC + 8.0 + 2.0 * TIGHT_ROW_H + 10.0);
        let s = &m.cards[m.index_of["Custom"] as usize];
        assert!(s.rows.is_empty());
        assert_eq!(s.h, HEADER_H_WITH_DESC + 8.0 + ROW_H_WITH_DESC + 10.0);
    }

    #[test]
    fn width_follows_the_estimate_formula() {
        let m = model_of(
            "type Query { averyveryverylongfieldnamehere: SomeQuiteLongTypeName }
             type SomeQuiteLongTypeName { x: String }",
            ModelOptions { today: "2020-01-01".into(), ..Default::default() },
        );
        let c = &m.cards[m.index_of["Query"] as usize];
        let expect = mono_w("averyveryverylongfieldnamehere", ROW_FONT_PX)
            + 16.0
            + mono_w("SomeQuiteLongTypeName", ROW_FONT_PX)
            + 20.0;
        assert_eq!(c.w, expect.max(MIN_CARD_W).round());
    }

    #[test]
    fn builtin_scalar_and_expired_types_are_colored_apart() {
        let m = model_of(
            "type Query { s: String, u: Other @deprecated(reason: \"gone [until 2000-01-01]\") }
             type Other { x: String }",
            ModelOptions { today: "2020-01-01".into(), ..Default::default() },
        );
        let c = &m.cards[m.index_of["Query"] as usize];
        assert_eq!(c.rows[0].type_color, TypeColor::BuiltinScalar);
        assert_eq!(c.rows[1].type_color, TypeColor::Expired);
        assert!(c.rows[1].until_expired);
    }

    #[test]
    fn hiding_primitives_remaps_rows_for_ports_and_pins() {
        let m = model_of(
            "type Query { s: String, o: Other } type Other { x: String }",
            ModelOptions {
                hide_primitive_fields: true,
                today: "2020-01-01".into(),
                ..Default::default()
            },
        );
        let c = &m.cards[m.index_of["Query"] as usize];
        assert_eq!(c.rows.len(), 1, "String field hidden");
        assert_eq!(c.display_row(0), None, "hidden field maps to nothing");
        assert_eq!(c.display_row(1), Some(0), "second field moved up");
    }
}
