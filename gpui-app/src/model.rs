//! View-model: node cards with geometry shared by painting and hit-testing.
//!
//! In the web app the card layout math existed three times (drawNodeSprite,
//! trailingSectionGeom, hitTestField) and drifted. Here `Card::row_y` /
//! `Card::row_at` are the single source of truth.

use gompass_core::graph::{
    all_reachable_ids, default_root_ops, reachable_from, EdgeKind, NodeKind, ParsedGraph,
};
use gompass_core::layout::{self, LayoutConfig, LayoutEdge, LayoutNode};
use std::collections::{HashMap, HashSet};

pub const HEADER_H: f32 = 42.0;
pub const ROW_H: f32 = 16.0;
pub const TOP_BODY_PAD: f32 = 8.0;
pub const BOTTOM_PAD: f32 = 10.0;
pub const CARD_PAD_X: f32 = 10.0;
pub const NAME_FONT_PX: f32 = 13.0;
pub const ROW_FONT_PX: f32 = 10.5;
/// Menlo advance width / font size.
pub const MONO_ADVANCE: f32 = 0.602;
pub const MIN_CARD_W: f32 = 220.0;
pub const MAX_CARD_W: f32 = 640.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Field,
    EnumValue,
    Implements,
    UnionMember,
    MemberOfUnion,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub kind: RowKind,
    pub left: String,
    /// Right-aligned type expression (empty for enum values / sections).
    pub right: String,
    /// Card index this row navigates to on click, if any.
    pub target: Option<u32>,
    pub deprecated: bool,
    pub is_overlay: bool,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Card {
    /// Index into `ParsedGraph::nodes` / `Model::cards`.
    pub index: u32,
    pub name: String,
    pub kind: NodeKind,
    pub kind_label: &'static str,
    pub rows: Vec<Row>,
    pub w: f32,
    pub h: f32,
    /// Whole type came from the overlay SDL.
    pub is_overlay: bool,
}

impl Card {
    pub fn row_y(&self, i: usize) -> f32 {
        HEADER_H + TOP_BODY_PAD + i as f32 * ROW_H
    }

    /// Inverse of `row_y` for hit-testing a point in card-local coords.
    pub fn row_at(&self, local_x: f32, local_y: f32) -> Option<usize> {
        if local_x < 0.0 || local_x > self.w {
            return None;
        }
        let body_y = local_y - HEADER_H - TOP_BODY_PAD;
        if body_y < 0.0 {
            return None;
        }
        let i = (body_y / ROW_H) as usize;
        (i < self.rows.len()).then_some(i)
    }

    pub fn port_y(&self, field_index: Option<usize>) -> f32 {
        match field_index {
            Some(i) if i < self.rows.len() => self.row_y(i) + ROW_H / 2.0,
            _ => self.h / 2.0,
        }
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
    /// Flattened bezier polyline in world coords: [x0,y0, x1,y1, ...].
    pub points: Vec<f32>,
    /// World-space bbox (min_x, min_y, max_x, max_y) for culling.
    pub bbox: [f32; 4],
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
pub fn slice_graph(full: &ParsedGraph, mode: Mode) -> ParsedGraph {
    let root_ops = root_ops_of(full);
    let (nodes, edges): (Vec<_>, Vec<_>) = match mode {
        Mode::Reachable => {
            let root = full
                .root_types
                .query
                .clone()
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

fn mono_w(text: &str, font_px: f32) -> f32 {
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

pub fn build_model(graph: ParsedGraph, schema_name: String) -> Model {
    let mut index_of: HashMap<String, u32> = HashMap::with_capacity(graph.nodes.len());
    for (i, n) in graph.nodes.iter().enumerate() {
        index_of.insert(n.id.clone(), i as u32);
    }

    // ---- cards ----
    let mut cards: Vec<Card> = Vec::with_capacity(graph.nodes.len());
    for (i, n) in graph.nodes.iter().enumerate() {
        let mut rows: Vec<Row> = Vec::new();
        for f in n.fields.as_deref().unwrap_or(&[]) {
            rows.push(Row {
                kind: RowKind::Field,
                left: f.name.clone(),
                right: f.type_.clone(),
                target: index_of.get(&f.type_name).copied(),
                deprecated: f.is_deprecated,
                is_overlay: f.is_overlay,
                description: f.description.clone(),
            });
        }
        for v in n.values.as_deref().unwrap_or(&[]) {
            rows.push(Row {
                kind: RowKind::EnumValue,
                left: v.name.clone(),
                right: String::new(),
                target: None,
                deprecated: v.is_deprecated,
                is_overlay: false,
                description: v.description.clone(),
            });
        }
        for m in n.members.as_deref().unwrap_or(&[]) {
            rows.push(Row {
                kind: RowKind::UnionMember,
                left: m.clone(),
                right: String::new(),
                target: index_of.get(m).copied(),
                deprecated: false,
                is_overlay: false,
                description: None,
            });
        }
        for iface in n.interfaces.as_deref().unwrap_or(&[]) {
            rows.push(Row {
                kind: RowKind::Implements,
                left: format!("implements {iface}"),
                right: String::new(),
                target: index_of.get(iface).copied(),
                deprecated: false,
                is_overlay: false,
                description: None,
            });
        }
        for u in n.member_of_unions.as_deref().unwrap_or(&[]) {
            rows.push(Row {
                kind: RowKind::MemberOfUnion,
                left: format!("in union {u}"),
                right: String::new(),
                target: index_of.get(u).copied(),
                deprecated: false,
                is_overlay: false,
                description: None,
            });
        }

        let mut w = mono_w(&n.name, NAME_FONT_PX) + mono_w(kind_label(n.kind), 9.0) + CARD_PAD_X * 3.0;
        for r in &rows {
            let row_w = mono_w(&r.left, ROW_FONT_PX)
                + if r.right.is_empty() { 0.0 } else { mono_w(&r.right, ROW_FONT_PX) + 24.0 }
                + CARD_PAD_X * 2.0;
            w = w.max(row_w);
        }
        let w = w.clamp(MIN_CARD_W, MAX_CARD_W);
        let h = HEADER_H + TOP_BODY_PAD + rows.len() as f32 * ROW_H + BOTTOM_PAD;

        cards.push(Card {
            index: i as u32,
            name: n.name.clone(),
            kind: n.kind,
            kind_label: kind_label(n.kind),
            rows,
            w,
            h,
            is_overlay: n.is_overlay,
        });
    }
    let overlay_marks = cards
        .iter()
        .map(|c| c.is_overlay as usize + c.rows.iter().filter(|r| r.is_overlay).count())
        .sum();

    // ---- layout ----
    let layout_nodes: Vec<LayoutNode> =
        cards.iter().map(|c| LayoutNode { w: c.w, h: c.h }).collect();
    let mut layout_edges: Vec<LayoutEdge> = Vec::with_capacity(graph.edges.len());
    let mut kept_edges: Vec<usize> = Vec::with_capacity(graph.edges.len());
    for (ei, e) in graph.edges.iter().enumerate() {
        let (Some(&from), Some(&to)) = (index_of.get(&e.source), index_of.get(&e.target)) else {
            continue;
        };
        if from == to {
            continue;
        }
        layout_edges.push(LayoutEdge {
            from,
            to,
            from_port_y: cards[from as usize].port_y(e.source_field_index),
        });
        kept_edges.push(ei);
    }
    let rt = &graph.root_types;
    let roots: Vec<u32> = [&rt.query, &rt.mutation, &rt.subscription]
        .into_iter()
        .filter_map(|name| name.as_ref().and_then(|n| index_of.get(n).copied()))
        .collect();
    let result = layout::layout(&layout_nodes, &layout_edges, &roots, &LayoutConfig::default());

    // ---- flatten edge paths ----
    const STEPS: usize = 16;
    let mut edges: Vec<EdgeVisual> = Vec::with_capacity(result.edges.len());
    for path in &result.edges {
        let le = &layout_edges[path.edge_index as usize];
        let ge = &graph.edges[kept_edges[path.edge_index as usize]];
        let group = edge_group(ge);
        let mut points = Vec::with_capacity((STEPS + 1) * 2);
        let mut bbox = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for s in 0..=STEPS {
            let t = s as f32 / STEPS as f32;
            let mt = 1.0 - t;
            let x = mt * mt * mt * path.start.x
                + 3.0 * mt * mt * t * path.c1.x
                + 3.0 * mt * t * t * path.c2.x
                + t * t * t * path.end.x;
            let y = mt * mt * mt * path.start.y
                + 3.0 * mt * mt * t * path.c1.y
                + 3.0 * mt * t * t * path.c2.y
                + t * t * t * path.end.y;
            points.push(x);
            points.push(y);
            bbox[0] = bbox[0].min(x);
            bbox[1] = bbox[1].min(y);
            bbox[2] = bbox[2].max(x);
            bbox[3] = bbox[3].max(y);
        }
        edges.push(EdgeVisual { from: le.from, to: le.to, group, points, bbox });
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
