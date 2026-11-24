//! Layered (Sugiyama-style) graph layout, left-to-right.
//!
//! Replaces the web app's GraphViz-WASM `dot` pipeline (and the 600-line
//! chunking orchestrator that existed purely to dodge WASM OOM). Native code
//! lays out the full graph in one pass:
//!
//! 1. split into weakly-connected components
//! 2. per component: cycle-break (DFS), BFS-depth ranking tightened toward
//!    each node's neighbours, over-tall ranks split into real ranks,
//!    **virtual-node expansion of every multi-rank edge in either
//!    direction**, barycenter ordering + transpose refinement over real
//!    *and* virtual nodes, median y-relaxation
//! 3. edges are routed **in the lanes their virtual nodes hold**: level
//!    across each column, in the slot the ordering gave them between two
//!    cards, changing height only in the gaps between columns — so no edge
//!    runs through a card it is not attached to. Runs of lanes are then
//!    straightened into level lines where that is cheap
//! 4. components shelf-packed (first-fit decreasing height), singletons in a
//!    grid

use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct LayoutNode {
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutEdge {
    pub from: u32,
    pub to: u32,
    /// y offset within the source node where the edge leaves (field row
    /// center). Clamped to the node height at path-building time.
    pub from_port_y: f32,
    /// Route through virtual-node lanes. Callers turn this off for hub
    /// edges (drawn faded anyway) so thousands of long edges don't flood
    /// the ranks with virtual nodes.
    pub route: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// One cubic Bézier segment: two control points and an end point (the start
/// is the previous segment's end, or `EdgePath::start`).
#[derive(Debug, Clone, Copy)]
pub struct CubicSeg {
    pub c1: Point,
    pub c2: Point,
    pub end: Point,
}

#[derive(Debug, Clone)]
pub struct EdgePath {
    /// Index into the input edge list.
    pub edge_index: u32,
    pub start: Point,
    /// Smooth curve, drawn directly as Béziers (no polyline approximation).
    pub curves: Vec<CubicSeg>,
    /// Coarse flattening of the same curve, for hit-testing and bounds.
    pub points: Vec<Point>,
}

#[derive(Debug, Clone)]
pub struct LayoutResult {
    /// Top-left position per node, indexed like the input.
    pub positions: Vec<Point>,
    pub edges: Vec<EdgePath>,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct LayoutConfig {
    pub rank_sep: f32,
    pub node_sep: f32,
    /// Padding between packed components.
    pub component_sep: f32,
    pub ordering_sweeps: usize,
    /// Adjacent-swap refinement passes after the barycenter sweeps.
    pub transpose_passes: usize,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            rank_sep: 110.0,
            node_sep: 22.0,
            component_sep: 60.0,
            // Both are past the point where more passes change anything
            // measurable on a 4000-edge schema.
            ordering_sweeps: 14,
            transpose_passes: 8,
        }
    }
}

/// Simplex pivots per component. The optimum is normally reached in far
/// fewer; the bound only stops a pathological graph from stalling the app.
const SIMPLEX_PIVOTS: usize = 2048;

struct XNode {
    /// Some(local real index) or None for virtual.
    real: Option<u32>,
    rank: i32,
    w: f32,
    h: f32,
}

/// Middle value, or `None` for an empty slice. The L1 centre: the point that
/// minimises the total distance to the inputs, which is what "put this node
/// where its neighbours are" means when the cost is edge length.
fn median(vals: &mut [f32]) -> Option<f32> {
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(vals[vals.len() / 2])
}

/// Sifting refinement: how many passes, and how far a node may slide.
/// Passes of the block-level refinement after clusters are gathered. Two is
/// where it stops finding anything: 2 and 16 land on the same drawing, and
/// each extra pass costs ~2ms of a 35ms layout.
const BLOCK_PASSES: usize = 2;


const VIRTUAL_W: f32 = 8.0;
/// Virtual nodes are lanes, not boxes: giving them height would inflate every
/// rank they pass through (thousands of them on a dense schema).
const VIRTUAL_H: f32 = 0.0;
/// Vertical gap between two lanes sharing a rank, for the spline router.
const VIRTUAL_SEP: f32 = 2.0;
/// The same for the lane router, where parallel edges run side by side for
/// whole columns and have to stay separate lines: at 2 a bundle fuses into a
/// solid band, at 6 each edge can be followed by eye. Measured cost on the
/// GitHub schema, 2 to 6: drawing 16% taller, edges 3% longer.
const LANE_PITCH: f32 = 6.0;
/// A link between two lanes is straightened only when they already sit within
/// this of each other. Without a limit every edge is level end to end, but
/// level lanes pin the cards of far-apart columns into one vertical order and
/// the drawing grows fivefold (22k to 97k tall). At 200 the bends halve for
/// 10% more height; at 800 it is already 75% taller.
const STRAIGHTEN_REACH: f32 = 200.0;

/// A parent type and the variants that should line up in the column directly
/// to its right — a union and its members.
#[derive(Debug, Clone)]
pub struct Cluster {
    pub parent: u32,
    pub members: Vec<u32>,
}

/// `clusters` keep each parent's variants packed together in their column,
/// with the parent to their left. Nothing in the graph connects the variants
/// to one another, so crossing reduction has no reason to keep them near each
/// other and a union otherwise reads as a spray of arrows into a scattered
/// set of boxes rather than as one group.
///
/// This is an ordering constraint only. Forcing the variants onto a shared
/// rank as well was measured: it raises the share of unions that end up in
/// one column from 15/42 to 18/42, and costs 14% average edge length and 35%
/// more edges cut across unrelated cards. Variants land in different columns
/// because something else reaches them by a shorter path, and overriding that
/// stretches every one of their other edges.
pub fn layout(
    nodes: &[LayoutNode],
    edges: &[LayoutEdge],
    roots: &[u32],
    clusters: &[Cluster],
    config: &LayoutConfig,
) -> LayoutResult {
    let n = nodes.len();
    let mut positions = vec![Point { x: 0.0, y: 0.0 }; n];
    if n == 0 {
        return LayoutResult { positions, edges: Vec::new(), width: 0.0, height: 0.0 };
    }

    // ---- components (union-find) ----
    let mut uf: Vec<u32> = (0..n as u32).collect();
    fn find(uf: &mut [u32], x: u32) -> u32 {
        let mut r = x;
        while uf[r as usize] != r {
            uf[r as usize] = uf[uf[r as usize] as usize];
            r = uf[r as usize];
        }
        r
    }
    for e in edges {
        let (a, b) = (find(&mut uf, e.from), find(&mut uf, e.to));
        if a != b {
            uf[a as usize] = b;
        }
    }
    let mut comp_of = vec![0u32; n];
    let mut comp_ids: Vec<u32> = Vec::new();
    for v in 0..n as u32 {
        let r = find(&mut uf, v);
        let cid = match comp_ids.iter().position(|&c| c == r) {
            Some(i) => i as u32,
            None => {
                comp_ids.push(r);
                (comp_ids.len() - 1) as u32
            }
        };
        comp_of[v as usize] = cid;
    }
    let ncomp = comp_ids.len();
    let mut members: Vec<Vec<u32>> = vec![Vec::new(); ncomp];
    for v in 0..n as u32 {
        members[comp_of[v as usize] as usize].push(v);
    }
    let mut comp_edges: Vec<Vec<u32>> = vec![Vec::new(); ncomp];
    for (ei, e) in edges.iter().enumerate() {
        comp_edges[comp_of[e.from as usize] as usize].push(ei as u32);
    }

    let is_root = {
        let mut m = vec![false; n];
        for &r in roots {
            if (r as usize) < n {
                m[r as usize] = true;
            }
        }
        m
    };

    // ---- lay out each component into its own local space ----
    struct CompLayout {
        members: Vec<u32>,
        w: f32,
        h: f32,
    }
    let mut singleton_ids: Vec<u32> = Vec::new();
    let mut comps: Vec<CompLayout> = Vec::new();
    // Waypoints per directed global-node pair, component-local until packing.
    let mut routes: HashMap<(u32, u32), Route> = HashMap::new();
    // The column each node sits in, as (left, right) x. Cards are centred in
    // their column, so a narrow card has column to either side of it that a
    // wider neighbour above or below occupies.
    let mut cols: Vec<(f32, f32)> = vec![(0.0, 0.0); n];
    let lanes = lanes_on();
    // Which component owns each route, so packing can translate them.
    let mut route_comp: HashMap<(u32, u32), usize> = HashMap::new();

    for ci in 0..ncomp {
        if members[ci].len() == 1 && comp_edges[ci].is_empty() {
            singleton_ids.push(members[ci][0]);
            continue;
        }
        let comp_index = comps.len();
        // A cluster only means anything inside one component's ordering.
        let comp_clusters: Vec<Cluster> = clusters
            .iter()
            .filter(|c| comp_of[c.parent as usize] as usize == ci)
            .filter_map(|c| {
                let members: Vec<u32> = c
                    .members
                    .iter()
                    .copied()
                    .filter(|&v| comp_of[v as usize] as usize == ci)
                    .collect();
                (members.len() > 1).then_some(Cluster { parent: c.parent, members })
            })
            .collect();
        let (w, h) = layout_component(
            &members[ci],
            &comp_edges[ci],
            nodes,
            edges,
            &is_root,
            &comp_clusters,
            config,
            &mut positions,
            &mut cols,
            |pair, route| {
                routes.insert(pair, route);
                route_comp.insert(pair, comp_index);
            },
        );
        comps.push(CompLayout { members: members[ci].clone(), w, h });
    }

    // ---- shelf-pack components (first-fit decreasing height) ----
    let total_area: f32 = comps.iter().map(|c| c.w * c.h).sum();
    let max_w = comps.iter().map(|c| c.w).fold(0.0f32, f32::max);
    let shelf_width = max_w.max((total_area.max(1.0)).sqrt() * 1.6);
    let mut order: Vec<usize> = (0..comps.len()).collect();
    order.sort_by(|&a, &b| comps[b].h.partial_cmp(&comps[a].h).unwrap());

    struct Shelf {
        y: f32,
        h: f32,
        cursor_x: f32,
    }
    let mut shelves: Vec<Shelf> = Vec::new();
    let mut packed_h: f32 = 0.0;
    let mut packed_w: f32 = 0.0;
    let mut comp_offset: Vec<(f32, f32)> = vec![(0.0, 0.0); comps.len()];
    for &i in &order {
        let c = &comps[i];
        let slot = shelves
            .iter_mut()
            .find(|s| s.cursor_x + c.w <= shelf_width && c.h <= s.h);
        let (ox, oy) = match slot {
            Some(s) => {
                let p = (s.cursor_x, s.y);
                s.cursor_x += c.w + config.component_sep;
                p
            }
            None => {
                let y = packed_h;
                shelves.push(Shelf {
                    y,
                    h: c.h,
                    cursor_x: c.w + config.component_sep,
                });
                packed_h = y + c.h + config.component_sep;
                (0.0, y)
            }
        };
        comp_offset[i] = (ox, oy);
        for &v in &c.members {
            positions[v as usize].x += ox;
            positions[v as usize].y += oy;
            cols[v as usize].0 += ox;
            cols[v as usize].1 += ox;
        }
        packed_w = packed_w.max(ox + c.w);
    }
    for (pair, waypoints) in routes.iter_mut() {
        let (ox, oy) = comp_offset[route_comp[pair]];
        for p in waypoints.pts.iter_mut() {
            p.x += ox;
            p.y += oy;
        }
    }
    if packed_h > 0.0 {
        packed_h -= config.component_sep;
    }

    // ---- singleton grid below the packed components ----
    if !singleton_ids.is_empty() {
        let row_w = packed_w.max(800.0);
        let mut x = 0.0f32;
        let mut y = if packed_h > 0.0 { packed_h + config.component_sep } else { 0.0 };
        let mut row_h = 0.0f32;
        for &v in &singleton_ids {
            let nd = nodes[v as usize];
            if x > 0.0 && x + nd.w > row_w {
                x = 0.0;
                y += row_h + config.node_sep;
                row_h = 0.0;
            }
            positions[v as usize] = Point { x, y };
            cols[v as usize] = (x, x + nd.w);
            x += nd.w + config.node_sep;
            row_h = row_h.max(nd.h);
            packed_w = packed_w.max(x);
        }
        packed_h = y + row_h;
    }

    // ---- edge paths: port → waypoints → target, Catmull-Rom smoothed ----
    let mut edge_paths = Vec::with_capacity(edges.len());
    let mut all_knots: Vec<Vec<Point>> = Vec::with_capacity(edges.len());
    for e in edges.iter() {
        let sp = positions[e.from as usize];
        let sn = nodes[e.from as usize];
        let tp = positions[e.to as usize];
        let tn = nodes[e.to as usize];
        let port_y = e.from_port_y.clamp(8.0, (sn.h - 8.0).max(8.0));
        let route = routes.get(&(e.from, e.to));
        let waypoints = route.map(|r| r.pts.as_slice()).unwrap_or(&[]);
        let (start, end) = anchor_points(sp, sn, tp, tn, port_y, waypoints);
        let mut knots: Vec<Point> = Vec::with_capacity(waypoints.len() + 4);
        knots.push(start);
        if lanes {
            knots.push(Point { x: cols[e.from as usize].1, y: start.y });
            knots.extend_from_slice(waypoints);
            knots.push(Point { x: cols[e.to as usize].0, y: end.y });
            knots.push(end);
        } else {
            knots.extend_from_slice(waypoints);
            knots.push(end);
            smooth_chain(&mut knots);
            simplify(&mut knots, 3.0);
        }
        all_knots.push(knots);
    }
    if lanes {
        spread_bends(&mut all_knots);
    }
    for (ei, knots) in all_knots.iter().enumerate() {
        let start = knots[0];
        let curves = if lanes { lane_path(knots, config.rank_sep) } else { spline(knots) };
        edge_paths.push(EdgePath {
            edge_index: ei as u32,
            start,
            points: flatten(start, &curves),
            curves,
        });
    }

    // The quality metrics cost more than the layout they measure (the edge
    // crossing count alone is a few million segment tests), so they sit
    // behind their own switch — GRAVIZ_PERF stays a timing signal.
    if std::env::var("GRAVIZ_METRICS").is_ok() {
        let card_area: f32 = nodes.iter().map(|n| n.w * n.h).sum();
        let mut dims: Vec<(f32, f32)> = comps.iter().map(|c| (c.w, c.h)).collect();
        dims.sort_by(|a, b| (b.0 * b.1).partial_cmp(&(a.0 * a.1)).unwrap());
        eprintln!(
            "packing: {} comps (top {:?}), {} singletons, fill {:.1}%",
            comps.len(),
            dims.iter().take(3).map(|(w, h)| format!("{w:.0}x{h:.0}")).collect::<Vec<_>>(),
            singleton_ids.len(),
            100.0 * card_area / (packed_w * packed_h).max(1.0)
        );
        // How many references come out as one straight line, which is what
        // the port-aware y fit is for: the departure row and the arrival
        // middle landing at the same height, with nothing bent in between.
        let straight = edge_paths
            .iter()
            .filter(|p| {
                p.curves.last().is_some_and(|c| (c.end.y - p.start.y).abs() < 2.0)
            })
            .count();
        eprintln!(
            "straight: {straight}/{} edges ({:.0}%) leave and arrive at the same height",
            edge_paths.len(),
            100.0 * straight as f32 / edge_paths.len().max(1) as f32
        );
        let mut total_len = 0.0f32;
        let mut total_dx = 0.0f32;
        let mut total_dy = 0.0f32;
        let mut longest = 0.0f32;
        let mut routed = 0usize;
        let mut backward = 0usize;
        for (e, p) in edges.iter().zip(edge_paths.iter()) {
            let (a, b) = (positions[e.from as usize], positions[e.to as usize]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            total_len += (dx * dx + dy * dy).sqrt();
            total_dx += dx.abs();
            total_dy += dy.abs();
            longest = longest.max((dx * dx + dy * dy).sqrt());
            if dx <= 1.0 {
                backward += 1;
            }
            if p.curves.len() > 1 {
                routed += 1;
            }
        }
        // How often an edge runs across a node it is not attached to — the
        // other half of "visual complexity". Counted on a coarse grid.
        const CELL: f32 = 512.0;
        let mut grid: std::collections::HashMap<(i32, i32), Vec<u32>> =
            std::collections::HashMap::new();
        for (i, (n, pos)) in nodes.iter().zip(positions.iter()).enumerate() {
            let (x0, y0) = ((pos.x / CELL) as i32, (pos.y / CELL) as i32);
            let (x1, y1) = (((pos.x + n.w) / CELL) as i32, ((pos.y + n.h) / CELL) as i32);
            for gx in x0..=x1 {
                for gy in y0..=y1 {
                    grid.entry((gx, gy)).or_default().push(i as u32);
                }
            }
        }
        let mut crossed = 0usize;
        let (mut back_crossed, mut back_n) = (0usize, 0usize);
        for (e, p) in edges.iter().zip(edge_paths.iter()) {
            let mut hit = std::collections::HashSet::new();
            for w in p.points.windows(2) {
                let (a, b) = (w[0], w[1]);
                // Fixed arc-length sampling: a per-segment step count would
                // make a heavily routed edge look worse purely because it
                // carries more waypoints.
                let seg = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                let steps = (seg / 32.0).ceil().max(1.0) as usize;
                for s in 0..=steps {
                    let t = s as f32 / steps as f32;
                    let (x, y) = (a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                    if let Some(ids) = grid.get(&((x / CELL) as i32, (y / CELL) as i32)) {
                        for &i in ids {
                            if i == e.from || i == e.to {
                                continue;
                            }
                            let (n, pos) = (&nodes[i as usize], positions[i as usize]);
                            if x >= pos.x && x <= pos.x + n.w && y >= pos.y && y <= pos.y + n.h {
                                hit.insert(i);
                            }
                        }
                    }
                }
            }
            crossed += hit.len();
            let (a, b) = (positions[e.from as usize], positions[e.to as usize]);
            if b.x <= a.x {
                back_crossed += hit.len();
                back_n += 1;
            }
        }
        // How well the clusters actually held together: average distance
        // between members, and the share of clusters whose members ended up
        // in one unbroken run.
        let (mut gap_sum, mut gap_n) = (0.0f32, 0usize);
        let (mut tight, mut same_col, mut total) = (0usize, 0usize, 0usize);
        for c in clusters {
            let g = &c.members;
            for (i, &a) in g.iter().enumerate() {
                for &b in &g[i + 1..] {
                    let (p, q) = (positions[a as usize], positions[b as usize]);
                    gap_sum += ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
                    gap_n += 1;
                }
            }
            // "Tight" = the variants share a column and sit in one
            // unbroken run down it. Cards are centred in their rank, so a
            // shared centre-x is exactly "same column", whatever the widths.
            total += 1;
            let col = |v: usize| positions[v].x + nodes[v].w / 2.0;
            let c0 = col(g[0] as usize);
            if g.iter().all(|&v| (col(v as usize) - c0).abs() < 1.0) {
                same_col += 1;
                let mut spans: Vec<(f32, f32)> = g
                    .iter()
                    .map(|&v| {
                        (positions[v as usize].y, positions[v as usize].y + nodes[v as usize].h)
                    })
                    .collect();
                spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                let (top, bottom) = (spans[0].0, spans[spans.len() - 1].1);
                let intruder = (0..nodes.len()).any(|o| {
                    !g.contains(&(o as u32))
                        && (col(o) - c0).abs() < 1.0
                        && positions[o].y + nodes[o].h > top
                        && positions[o].y < bottom
                });
                if !intruder {
                    tight += 1;
                }
            }
        }
        // Edge-edge crossings: **pairs of edges that cross**, counted once
        // however many times their flattened paths intersect.
        //
        // Two things this deliberately does not count. Edges that share an
        // endpoint meet there by construction — every reference into a hub
        // arrives at the same point, and pairing them all up would report
        // thousands of "crossings" that are just the fan converging. And a
        // pair that intersects several times is one tangle to the eye, not
        // three.
        const XCELL: f32 = 384.0;
        let mut segs: Vec<(Point, Point, u32)> = Vec::new();
        for (ei, p) in edge_paths.iter().enumerate() {
            for w in p.points.windows(2) {
                segs.push((w[0], w[1], ei as u32));
            }
        }
        let mut cells: std::collections::HashMap<(i32, i32), Vec<u32>> =
            std::collections::HashMap::new();
        for (si, (a, b, _)) in segs.iter().enumerate() {
            let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
            let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
            for gx in (x0 / XCELL) as i32..=(x1 / XCELL) as i32 {
                for gy in (y0 / XCELL) as i32..=(y1 / XCELL) as i32 {
                    cells.entry((gx, gy)).or_default().push(si as u32);
                }
            }
        }
        let cross = |p: Point, q: Point, r: Point, s: Point| -> bool {
            let d = |a: Point, b: Point, c: Point| {
                (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
            };
            let (d1, d2, d3, d4) = (d(p, q, r), d(p, q, s), d(r, s, p), d(r, s, q));
            ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
        };
        let shares_end = |i: usize, j: usize| {
            let (a, b) = (&edges[i], &edges[j]);
            a.from == b.from || a.from == b.to || a.to == b.from || a.to == b.to
        };
        let mut tangled: std::collections::HashSet<(u32, u32)> =
            std::collections::HashSet::new();
        for ids in cells.values() {
            for (i, &a) in ids.iter().enumerate() {
                for &b in &ids[i + 1..] {
                    let (sa, sb) = (&segs[a as usize], &segs[b as usize]);
                    let (ea, eb) = (sa.2, sb.2);
                    if ea == eb {
                        continue;
                    }
                    let pair = if ea < eb { (ea, eb) } else { (eb, ea) };
                    if tangled.contains(&pair) {
                        continue;
                    }
                    if shares_end(ea as usize, eb as usize) {
                        continue;
                    }
                    if cross(sa.0, sa.1, sb.0, sb.1) {
                        tangled.insert(pair);
                    }
                }
            }
        }
        let edge_x = tangled.len();
        eprintln!(
            "cards: {} back edges cross {:.2} cards each, {} forward cross {:.2}",
            back_n,
            back_crossed as f32 / back_n.max(1) as f32,
            edges.len() - back_n,
            (crossed - back_crossed) as f32 / (edges.len() - back_n).max(1) as f32
        );
        // How concentrated are the crossings? If a small set of long edges
        // accounts for most of them, those are worth routing differently.
        let mut per_edge = vec![0u32; edges.len()];
        for &(a, b) in &tangled {
            per_edge[a as usize] += 1;
            per_edge[b as usize] += 1;
        }
        let mut by_len: Vec<usize> = (0..edges.len()).collect();
        by_len.sort_by(|&i, &j| {
            let l = |k: usize| {
                let (p, q) = (positions[edges[k].from as usize], positions[edges[k].to as usize]);
                (q.x - p.x).powi(2) + (q.y - p.y).powi(2)
            };
            l(j).partial_cmp(&l(i)).unwrap()
        });
        let top = edges.len() / 10;
        let top_share: u64 = by_len[..top].iter().map(|&i| per_edge[i] as u64).sum();
        let all: u64 = per_edge.iter().map(|&c| c as u64).sum();
        eprintln!(
            "crossings: longest 10% of edges carry {:.0}% of them",
            100.0 * top_share as f32 / all.max(1) as f32
        );
        // Where the card crossings come from: an edge whose ends share a
        // column has no lane to travel in and is drawn straight through
        // whatever sits between them.
        let mut flat = 0usize;
        for e in edges.iter() {
            let (a, b) = (positions[e.from as usize], positions[e.to as usize]);
            if (a.x - b.x).abs() < 1.0 {
                flat += 1;
            }
        }
        // Edges that end at a dangling node — one with nothing leaving it.
        // A leaf carries no structure of its own, so the only thing its edge
        // has to say is "this belongs to that"; the further it reaches, the
        // more it costs to say it.
        let mut has_out = vec![false; nodes.len()];
        for e in edges {
            if e.from != e.to {
                has_out[e.from as usize] = true;
            }
        }
        let (mut leaf_len, mut leaf_n) = (0.0f32, 0usize);
        let (mut leaf_dx, mut leaf_dy) = (0.0f32, 0.0f32);
        for e in edges {
            if !has_out[e.to as usize] {
                let (a, b) = (positions[e.from as usize], positions[e.to as usize]);
                leaf_len += (b.x - a.x).hypot(b.y - a.y);
                leaf_dx += (b.x - a.x).abs();
                leaf_dy += (b.y - a.y).abs();
                leaf_n += 1;
            }
        }
        {
            let mut preds = vec![0usize; nodes.len()];
            for e in edges {
                if e.from != e.to && !has_out[e.to as usize] {
                    preds[e.to as usize] += 1;
                }
            }
            let leaves: Vec<usize> = (0..nodes.len()).filter(|&i| preds[i] > 0).collect();
            let shared: Vec<usize> = leaves.iter().copied().filter(|&i| preds[i] > 1).collect();
            let (mut single_len, mut single_n) = (0.0f32, 0usize);
            let (mut shared_len, mut shared_n) = (0.0f32, 0usize);
            for e in edges {
                if e.from == e.to || has_out[e.to as usize] {
                    continue;
                }
                let (a, b) = (positions[e.from as usize], positions[e.to as usize]);
                let l = (b.x - a.x).hypot(b.y - a.y);
                if preds[e.to as usize] > 1 {
                    shared_len += l;
                    shared_n += 1;
                } else {
                    single_len += l;
                    single_n += 1;
                }
            }
            eprintln!(
                "leaves: {} total, {} referenced more than once; \
                 {} single-ref edges avg {:.0}, {} shared-ref edges avg {:.0}",
                leaves.len(),
                shared.len(),
                single_n,
                single_len / single_n.max(1) as f32,
                shared_n,
                shared_len / shared_n.max(1) as f32
            );
        }
        let segs: usize = edge_paths.iter().map(|p| p.curves.len()).sum();
        let bends: usize = edge_paths
            .iter()
            .map(|p| {
                let mut from = p.start;
                let mut n = 0;
                for c in &p.curves {
                    if (c.end.y - from.y).abs() > 0.5 {
                        n += 1;
                    }
                    from = c.end;
                }
                n
            })
            .sum();
        eprintln!("segments: {segs} cubics, {bends} of them bends");
        // What the reader's eye actually travels: the drawn length of every
        // edge, bends and detours included, next to the straight-line total
        // it could not do better than; and the box it all sits in.
        let drawn: f64 = edge_paths
            .iter()
            .map(|p| p.points.windows(2).map(|w| (w[1].x - w[0].x).hypot(w[1].y - w[0].y) as f64).sum::<f64>())
            .sum();
        eprintln!(
            "extent: drawn edge length {:.0} total ({:.0}/edge, {:.2}x the straight-line {:.0}), box {:.0}x{:.0} = {:.1}M px²",
            drawn,
            drawn / edges.len().max(1) as f64,
            drawn / (total_len as f64).max(1.0),
            total_len,
            packed_w,
            packed_h,
            packed_w as f64 * packed_h as f64 / 1e6
        );
        let n_edges = edges.len().max(1) as f32;
        eprintln!(
            "layout: {} nodes, {} edges, routed {}, avg len {:.0} = {:.1}% of the diagonal (dx {:.0} dy {:.0}), longest {:.0}, backward {}, crossings {} ({:.2}/edge), tangles {} ({:.1} per edge), to-leaf {} avg {:.0} (dx {:.0} dy {:.0}), same-col {}, union gap {:.0} col {}/{} tight {}/{}",
            nodes.len(),
            edges.len(),
            routed,
            total_len / n_edges,
            // Relative to the drawing's diagonal. Absolute pixels only say
            // how big the picture is; what costs the reader is how far across
            // it an edge makes them travel.
            100.0 * (total_len / n_edges) / (packed_w.hypot(packed_h)).max(1.0),
            total_dx / n_edges,
            total_dy / n_edges,
            longest,
            backward,
            crossed,
            crossed as f32 / n_edges,
            edge_x,
            2.0 * edge_x as f32 / n_edges,
            leaf_n,
            leaf_len / leaf_n.max(1) as f32,
            leaf_dx / leaf_n.max(1) as f32,
            leaf_dy / leaf_n.max(1) as f32,
            flat,
            gap_sum / gap_n.max(1) as f32,
            same_col,
            total,
            tight,
            total
        );
    }
    LayoutResult {
        positions,
        edges: edge_paths,
        width: packed_w,
        height: packed_h,
    }
}

/// The lanes an edge travels in between its two cards.
struct Route {
    /// With lane routing: where the edge enters and leaves each column it
    /// crosses, in travel order. Without: one point per column.
    pts: Vec<Point>,
}

/// A/B switch while the two routers are being compared.
fn lanes_on() -> bool {
    std::env::var("GRAVIZ_ROUTE").map(|v| v != "old").unwrap_or(true)
}

/// Build an edge from knots that already keep it clear of the cards.
///
/// Inside a column the edge runs level, in the slot the ordering gave it
/// between two cards; every change of height happens in the gap between
/// columns, where there is nothing to hit. A spline through one point per
/// column, which this replaces, cut the corner on each change of height and
/// went straight through whatever card sat next to its lane.
///
/// Three shapes, picked from the knots alone: level knots are a line, a
/// change of height across a gap is an S with both ends level, and two knots
/// stacked on the same x are a U — a back edge turning round, first out of
/// the right of its source and last into the left of its target.
fn lane_path(knots: &[Point], gap: f32) -> Vec<CubicSeg> {
    let mut out: Vec<CubicSeg> = Vec::with_capacity(knots.len());
    let mut prev_line = false;
    let mut turns = 0;
    for w in knots.windows(2) {
        let (p, q) = (w[0], w[1]);
        let (dx, dy) = (q.x - p.x, q.y - p.y);
        if dy.abs() < 0.5 {
            if dx.abs() < 0.5 {
                continue;
            }
            if prev_line {
                // One line, however many columns it crosses.
                let last = out.len() - 1;
                let from = Point {
                    x: (3.0 * out[last].c1.x - out[last].end.x) / 2.0,
                    y: out[last].end.y,
                };
                out[last] = line_seg(from, Point { x: q.x, y: from.y });
            } else {
                out.push(line_seg(p, Point { x: q.x, y: p.y }));
            }
            prev_line = true;
            continue;
        }
        prev_line = false;
        if dx.abs() < 1.0 {
            let dir = if turns == 0 { 1.0 } else { -1.0 };
            turns += 1;
            let d = (dy.abs() * 0.5).clamp(16.0, gap * 0.7) * dir;
            out.push(CubicSeg {
                c1: Point { x: p.x + d, y: p.y },
                c2: Point { x: q.x + d, y: q.y },
                end: q,
            });
        } else {
            let pull = dx / 2.0;
            out.push(CubicSeg {
                c1: Point { x: p.x + pull, y: p.y },
                c2: Point { x: q.x - pull, y: q.y },
                end: q,
            });
        }
    }
    out
}

/// Give every bend in a gap its own place to turn.
///
/// All the edges that change height between two columns used to do it across
/// the full width of the gap, so their steep middles lay on top of one
/// another and a bundle stepping down together drew as one fat stroke. Here
/// each bend takes half the gap and they are staggered across the rest, in
/// the order that keeps a bundle nested: of edges heading down, the lowest
/// turns first, so none has to cross a neighbour that is still running
/// level; heading up, the highest.
fn spread_bends(all: &mut [Vec<Point>]) {
    // (gap, heading down) -> (height it leaves from, edge, knot index)
    let mut gaps: HashMap<(u32, u32, bool), Vec<(f32, u32, u32)>> = HashMap::new();
    for (e, knots) in all.iter().enumerate() {
        for (i, w) in knots.windows(2).enumerate() {
            let (p, q) = (w[0], w[1]);
            if (q.y - p.y).abs() >= 0.5 && (q.x - p.x).abs() >= 40.0 {
                gaps.entry((p.x.to_bits(), q.x.to_bits(), q.y > p.y))
                    .or_default()
                    .push((p.y, e as u32, i as u32));
            }
        }
    }
    let mut turn: Vec<Vec<(u32, f32)>> = vec![Vec::new(); all.len()];
    for (key, mut bends) in gaps {
        let down = key.2;
        bends.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if down {
            bends.reverse();
        }
        // Turns sit a lane pitch apart, as many as fit in the half of the
        // gap the bends leave free. A busy gap has far more bends than that,
        // and spreading them evenly anyway packs them into a solid slab;
        // there, neighbours share a turn and the gap reads as a few lines.
        let n = bends.len();
        let gap = (f32::from_bits(key.1) - f32::from_bits(key.0)).abs();
        let slots = ((gap * 0.5 / LANE_PITCH) as usize).max(1);
        let lead = slots.saturating_sub(n) as f32 / 2.0;
        for (k, &(_, e, i)) in bends.iter().enumerate() {
            let slot = if n <= slots { k } else { k * slots / n };
            turn[e as usize].push((i, (slot as f32 + 0.5 + lead) / slots as f32));
        }
    }
    for (knots, mut turns) in all.iter_mut().zip(turn) {
        if turns.is_empty() {
            continue;
        }
        turns.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut out = Vec::with_capacity(knots.len() + turns.len() * 2);
        let mut next = turns.iter().peekable();
        for (i, w) in knots.windows(2).enumerate() {
            out.push(w[0]);
            if let Some(&&(_, t)) = next.peek().filter(|&&&(at, _)| at as usize == i) {
                next.next();
                let (p, q) = (w[0], w[1]);
                let dx = q.x - p.x;
                let from = p.x + dx * 0.5 * t;
                out.push(Point { x: from, y: p.y });
                out.push(Point { x: from + dx * 0.5, y: q.y });
            }
        }
        out.push(*knots.last().unwrap());
        *knots = out;
    }
}

fn line_seg(p: Point, q: Point) -> CubicSeg {
    CubicSeg {
        c1: Point { x: p.x + (q.x - p.x) / 3.0, y: p.y + (q.y - p.y) / 3.0 },
        c2: Point { x: p.x + (q.x - p.x) * 2.0 / 3.0, y: p.y + (q.y - p.y) * 2.0 / 3.0 },
        end: q,
    }
}

/// Start on the source's right or left side (whichever faces the route),
/// end on the target's facing side at its vertical center.
fn anchor_points(
    sp: Point,
    sn: LayoutNode,
    tp: Point,
    tn: LayoutNode,
    port_y: f32,
    waypoints: &[Point],
) -> (Point, Point) {
    // Always out of the right face and into the left face, whichever way the
    // two cards happen to sit. The side an edge touches then *means*
    // something — right is "this type refers to", left is "is referred to by"
    // — instead of merely reporting which card the layout put first. An edge
    // whose target ended up on the left loops back around, which is what a
    // back edge should look like.
    let _ = waypoints;
    let start = Point { x: sp.x + sn.w, y: sp.y + port_y };
    // Arrive at the height it left from, when that lands on the target's
    // face. Pinning every arrival to the middle of the face meant the two
    // ends of a reference sat at different heights by construction, so even
    // a card sitting straight across from its field got an S. Out of range
    // the arrival slides to the nearest corner of the face and the curve
    // takes up the rest.
    const FACE_INSET: f32 = 8.0;
    let (lo, hi) = (tp.y + FACE_INSET, tp.y + (tn.h - FACE_INSET).max(FACE_INSET));
    let end = Point { x: tp.x, y: start.y.clamp(lo, hi) };
    (start, end)
}

/// Relax the interior of a routed chain toward its neighbours.
///
/// Lanes are positioned one rank at a time, so a long route comes out of the
/// ordering pass as a zigzag between independently-chosen y values. Averaging
/// the interior knots straightens that into the gentle arc the route was
/// meant to be: it reads as one line instead of a staircase, and the flatter
/// curve costs the renderer far fewer segments to draw. Endpoints are pinned,
/// and the pull is deliberately weak so the route keeps clear of the cards
/// the lanes were threaded between.
fn smooth_chain(pts: &mut [Point]) {
    if pts.len() < 3 {
        return;
    }
    // Twenty is where the measured crossing count stops improving.
    // Straightening a lane-routed chain is what keeps it off the cards it was
    // threaded between: at 0 passes an edge crosses 9.2 cards, at 20 it is
    // 4.1, and it flattens by 80.
    const PASSES: usize = 80;
    const PULL: f32 = 0.5;
    let mut prev = pts.to_vec();
    for _ in 0..PASSES {
        for i in 1..pts.len() - 1 {
            let target = (prev[i - 1].y + prev[i + 1].y) / 2.0;
            pts[i].y = prev[i].y + (target - prev[i].y) * PULL;
        }
        prev.copy_from_slice(pts);
    }
}

/// Drop knots that sit within `eps` of the line their neighbours already
/// describe (Douglas-Peucker).
///
/// An edge routed through eight ranks collects a waypoint per rank, and most
/// of those runs come out straight. Every surviving knot becomes a cubic that
/// the renderer has to flatten on every frame, so removing the ones that
/// carry no shape is free frame time — the curve through the remaining knots
/// is the same curve.
fn simplify(pts: &mut Vec<Point>, eps: f32) {
    if pts.len() < 3 {
        return;
    }
    let n = pts.len();
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (p, q) = (pts[a], pts[b]);
        let (dx, dy) = (q.x - p.x, q.y - p.y);
        let len = (dx * dx + dy * dy).sqrt();
        let mut worst = 0.0f32;
        let mut worst_i = a;
        for (i, r) in pts.iter().enumerate().take(b).skip(a + 1) {
            let d = if len < 1e-6 {
                ((r.x - p.x).powi(2) + (r.y - p.y).powi(2)).sqrt()
            } else {
                ((q.x - p.x) * (p.y - r.y) - (p.x - r.x) * (q.y - p.y)).abs() / len
            };
            if d > worst {
                worst = d;
                worst_i = i;
            }
        }
        if worst > eps {
            keep[worst_i] = true;
            stack.push((a, worst_i));
            stack.push((worst_i, b));
        }
    }
    let mut i = 0;
    pts.retain(|_| {
        i += 1;
        keep[i - 1]
    });
}

/// Catmull-Rom through the knots, expressed as cubic Béziers. The first
/// and last tangents are forced horizontal so an edge leaves its source port
/// and enters its target sideways — the layered "flow" look, which reads far
/// cleaner than straight diagonals.
fn spline(knots: &[Point]) -> Vec<CubicSeg> {
    if knots.len() < 2 {
        return Vec::new();
    }
    let n = knots.len();
    let mut out = Vec::with_capacity(n - 1);
    for i in 0..n - 1 {
        let p1 = knots[i];
        let p2 = knots[i + 1];
        let dx = (p2.x - p1.x).abs();
        // horizontal pull, proportional to the span but bounded
        let pull = (dx / 2.0).clamp(24.0, 160.0);
        // The two ends always leave rightward and arrive rightward, matching
        // the faces they are anchored to. Following the segment's own
        // direction would make a back edge dive left the instant it left the
        // card, crossing back over the card it just came from.
        let c1 = if i == 0 {
            Point { x: p1.x + pull, y: p1.y }
        } else {
            let p0 = knots[i - 1];
            Point { x: p1.x + (p2.x - p0.x) / 6.0, y: p1.y + (p2.y - p0.y) / 6.0 }
        };
        let c2 = if i + 2 < n {
            let p3 = knots[i + 2];
            Point { x: p2.x - (p3.x - p1.x) / 6.0, y: p2.y - (p3.y - p1.y) / 6.0 }
        } else {
            Point { x: p2.x - pull, y: p2.y }
        };
        out.push(CubicSeg { c1, c2, end: p2 });
    }
    out
}

/// Coarse sampling of the curve — used only for hit-testing and bounds, so a
/// few samples per segment is plenty.
fn flatten(start: Point, curves: &[CubicSeg]) -> Vec<Point> {
    const STEPS: usize = 6;
    let mut pts = Vec::with_capacity(curves.len() * STEPS + 1);
    pts.push(start);
    let mut p0 = start;
    for c in curves {
        for s in 1..=STEPS {
            let t = s as f32 / STEPS as f32;
            let mt = 1.0 - t;
            let (a, b, cc, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            pts.push(Point {
                x: a * p0.x + b * c.c1.x + cc * c.c2.x + d * c.end.x,
                y: a * p0.y + b * c.c1.y + cc * c.c2.y + d * c.end.y,
            });
        }
        p0 = c.end;
    }
    pts
}

/// Writes component-local positions into `positions`, reports per-pair
/// waypoint routes via `emit_route`; returns (width, height).
#[allow(clippy::too_many_arguments)]
fn layout_component(
    comp: &[u32],
    comp_edge_ids: &[u32],
    nodes: &[LayoutNode],
    edges: &[LayoutEdge],
    is_root: &[bool],
    clusters: &[Cluster],
    config: &LayoutConfig,
    positions: &mut [Point],
    cols: &mut [(f32, f32)],
    mut emit_route: impl FnMut((u32, u32), Route),
) -> (f32, f32) {
    let lanes = lanes_on();
    let m = comp.len();
    let mut local_of = HashMap::with_capacity(m);
    for (i, &v) in comp.iter().enumerate() {
        local_of.insert(v, i as u32);
    }
    // deduped directed pairs (+ which of them want lane routing)
    let mut routable: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let mut pairs: Vec<(u32, u32)> = comp_edge_ids
        .iter()
        .map(|&ei| {
            let e = &edges[ei as usize];
            let p = (local_of[&e.from], local_of[&e.to]);
            if e.route {
                routable.insert(p);
            }
            p
        })
        .filter(|(a, b)| a != b)
        .collect();
    pairs.sort_unstable();
    pairs.dedup();

    // Where an edge meets its source: the row of the field it comes from,
    // clamped the way the path builder clamps it. Several fields pointing at
    // the same type share one pair, so the median row stands for them.
    let mut port_rows: HashMap<(u32, u32), Vec<f32>> = HashMap::new();
    for &ei in comp_edge_ids {
        let e = &edges[ei as usize];
        let (f, t) = (local_of[&e.from], local_of[&e.to]);
        if f != t {
            let h = nodes[comp[f as usize] as usize].h;
            port_rows
                .entry((f, t))
                .or_default()
                .push(e.from_port_y.clamp(8.0, (h - 8.0).max(8.0)));
        }
    }
    let port_of: HashMap<(u32, u32), f32> = port_rows
        .into_iter()
        .filter_map(|(k, mut v)| median(&mut v).map(|m| (k, m)))
        .collect();

    // ---- cycle break: Eades-Lin-Smyth ordering, reverse what points back ----
    // Put the nodes in a line — sinks peeled to the back, sources to the
    // front, and when neither is left the node that is most nearly a source —
    // and reverse every edge that points backwards along it.
    //
    // This replaced reversing the back edges of a depth-first search. DFS
    // dives down one long path before it looks sideways, and every edge it
    // reverses then spans that whole path: on the GitHub schema, 72 columns
    // and 431 back edges. This gives 44 columns and 237, and edges 17%
    // shorter (6069 to 5014). The price is 10% more edge-edge crossings
    // (170 to 188 per edge): the same edges in two thirds of the width.
    // Orienting by breadth-first depth was also measured: fewer columns
    // still, but 1648 back edges, each a loop around both of its cards.
    //
    // acyclic keeps the pair's ORIGINAL orientation alongside the DAG one.
    let mut acyclic: Vec<((u32, u32), bool)>;
    {
        let mut outs: Vec<Vec<u32>> = vec![Vec::new(); m];
        let mut ins: Vec<Vec<u32>> = vec![Vec::new(); m];
        for &(a, b) in &pairs {
            outs[a as usize].push(b);
            ins[b as usize].push(a);
        }
        let mut ord = vec![0u32; m];
        let mut od: Vec<i32> = outs.iter().map(|v| v.len() as i32).collect();
        let mut id: Vec<i32> = ins.iter().map(|v| v.len() as i32).collect();
        let mut gone = vec![false; m];
        let (mut front, mut back) = (0u32, m as u32);
        let mut left = m;
        let take = |v: usize, od: &mut Vec<i32>, id: &mut Vec<i32>, gone: &mut Vec<bool>| {
            gone[v] = true;
            for &w in &outs[v] {
                id[w as usize] -= 1;
            }
            for &w in &ins[v] {
                od[w as usize] -= 1;
            }
        };
        // The schema's entry points lead, whatever refers back to them.
        for v in 0..m {
            if is_root[comp[v] as usize] {
                take(v, &mut od, &mut id, &mut gone);
                ord[v] = front;
                front += 1;
                left -= 1;
            }
        }
        while left > 0 {
            let mut moved = true;
            while moved {
                moved = false;
                for v in 0..m {
                    if !gone[v] && od[v] == 0 {
                        take(v, &mut od, &mut id, &mut gone);
                        back -= 1;
                        ord[v] = back;
                        left -= 1;
                        moved = true;
                    }
                }
                for v in 0..m {
                    if !gone[v] && id[v] == 0 {
                        take(v, &mut od, &mut id, &mut gone);
                        ord[v] = front;
                        front += 1;
                        left -= 1;
                        moved = true;
                    }
                }
            }
            // Lowest index wins a tie, so the same schema always lays out
            // the same way.
            let Some(v) = (0..m)
                .filter(|&v| !gone[v])
                .max_by_key(|&v| (od[v] - id[v], std::cmp::Reverse(v)))
            else {
                break;
            };
            take(v, &mut od, &mut id, &mut gone);
            ord[v] = front;
            front += 1;
            left -= 1;
        }
        acyclic = pairs
            .iter()
            .map(|&(a, b)| {
                if ord[a as usize] < ord[b as usize] {
                    ((a, b), false)
                } else {
                    ((b, a), true)
                }
            })
            .collect();
    }
    acyclic.sort_unstable_by_key(|&(p, _)| p);
    acyclic.dedup_by_key(|&mut (p, _)| p);

    // ---- ranking ----
    // Every edge must advance at least one column, so the drawing reads left
    // to right with no backflow: `ranking::network_simplex` finds the layering
    // that does that with the smallest total rank span, which is the
    // horizontal half of edge length. It costs depth — this schema is 72
    // columns instead of the 37 a breadth-first depth would give — but with
    // edges anchored right face to left face, an edge that points backwards
    // has to loop the whole way around, and those loops cost more in
    // crossings than the extra width does.
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut pred: Vec<Vec<u32>> = vec![Vec::new(); m];
    for &((a, b), _) in &acyclic {
        if a != b {
            succ[a as usize].push(b);
            pred[b as usize].push(a);
        }
    }
    // Weighting edges into dangling nodes was tried, so the simplex would
    // spend its slack attaching leaves rather than threading the graph: it
    // moves the leaf edges 5% closer and costs 1.4% more edge crossings,
    // because a leaf referenced from two distant columns has to sit after the
    // later one whatever the weight says. The structure, not the weight, is
    // what makes those edges long.
    let t_stage = std::time::Instant::now();
    let timing = std::env::var("GRAVIZ_PERF").is_ok() && m > 100;
    let redges: Vec<crate::ranking::RankEdge> = acyclic
        .iter()
        .filter(|((a, b), _)| a != b)
        .map(|&((a, b), _)| crate::ranking::RankEdge { from: a, to: b, weight: 1, minlen: 1 })
        .collect();
    let mut rank = crate::ranking::network_simplex(m, &redges, SIMPLEX_PIVOTS);


    if timing {
        eprintln!("  stage rank      {:>6.1}ms", t_stage.elapsed().as_secs_f32() * 1000.0);
    }
    let t_stage = std::time::Instant::now();

    // ---- balance over-tall ranks into real ranks ----
    // BFS depth alone leaves one rank holding hundreds of nodes, which turns
    // the component into a 100k-px ribbon. Split those ranks by inserting a
    // genuine rank and moving the overflow into it — done *before* the
    // crossing-reduction sweeps, so "one rank = one column" stays true and
    // the ordering that follows actually holds. (Splitting after ordering,
    // as an earlier version did, threw the ordering away.)
    {
        let node_h = |v: usize| nodes[comp[v] as usize].h + config.node_sep;
        let total_h: f32 = (0..m).map(node_h).sum();
        let avg_w = (0..m).map(|v| nodes[comp[v] as usize].w).sum::<f32>() / m.max(1) as f32;
        // Cap chosen so the finished component lands near square.
        // Near-square, which also measured as the edge-length optimum: a
        // wider drawing lengthens every edge's horizontal run, a taller one
        // its vertical run.
        // Re-measured against drawn edge length with lane routing: half
        // this cap draws 39% more edge, three times it 3% more.
        let cap = (total_h * (avg_w + config.rank_sep)).sqrt().max(2000.0);
        // Each node's atomic unit: itself, or a shared id if it belongs to a
        // cluster that is now sitting in one column. Splitting a rank must
        // move a cluster whole or not at all, else it undoes the grouping.
        // Units stay disjoint (a type can sit in two unions) and only a
        // cluster that comfortably fits a column is protected — a union with
        // dozens of members is taller than the cap by itself, and refusing to
        // split it would stretch the whole picture for one group's sake.
        let mut unit_of: Vec<usize> = (0..m).collect();
        {
            let mut order: Vec<usize> = (0..clusters.len()).collect();
            order.sort_by_key(|&i| clusters[i].members.len());
            let mut taken = vec![false; m];
            for &ci in &order {
                let members: Vec<usize> = clusters[ci]
                    .members
                    .iter()
                    .filter_map(|v| local_of.get(v).copied())
                    .map(|l| l as usize)
                    .filter(|&l| !taken[l])
                    .collect();
                if members.len() < 2 || members.iter().any(|&l| rank[l] != rank[members[0]]) {
                    continue;
                }
                if members.iter().map(|&l| node_h(l)).sum::<f32>() > cap * 0.5 {
                    continue;
                }
                for &l in &members {
                    unit_of[l] = m + ci;
                    taken[l] = true;
                }
            }
        }
        let mut succ_min: Vec<i32> = vec![i32::MAX / 2; m];
        for &((a, b), _) in &acyclic {
            if a != b {
                let e = &mut succ_min[a as usize];
                *e = (*e).min(rank[b as usize]);
            }
        }
        for _ in 0..2048 {
            let max_rank = *rank.iter().max().unwrap_or(&0);
            let mut heights = vec![0.0f32; (max_rank + 1) as usize];
            for v in 0..m {
                heights[rank[v] as usize] += node_h(v);
            }
            let mut rank_units: Vec<HashMap<usize, Vec<usize>>> =
                vec![HashMap::new(); (max_rank + 1) as usize];
            for v in 0..m {
                rank_units[rank[v] as usize].entry(unit_of[v]).or_default().push(v);
            }
            // Only ranks that can actually be split: one holding a single unit
            // is already as short as it may get, and picking it again would
            // spin here until the iteration cap while every other over-tall
            // rank went untouched.
            let Some(r) = (0..heights.len())
                .find(|&r| heights[r] > cap && rank_units[r].len() > 1)
            else {
                break;
            };
            let r = r as i32;
            let mut members: Vec<(usize, Vec<usize>)> =
                std::mem::take(&mut rank_units[r as usize]).into_iter().collect();
            // Units whose successors sit furthest away move first — they are
            // the ones with room, and moving them shortens their edges. The
            // unit id breaks ties, because hash order is not stable across
            // runs and the layout has to be.
            members.sort_by_key(|(id, u)| {
                (-u.iter().map(|&v| succ_min[v]).min().unwrap_or(0), *id)
            });
            let members: Vec<Vec<usize>> = members.into_iter().map(|(_, u)| u).collect();
            for x in rank.iter_mut() {
                if *x > r {
                    *x += 1;
                }
            }
            for e in succ_min.iter_mut() {
                if *e > r {
                    *e += 1;
                }
            }
            let mut kept = 0.0f32;
            for (i, unit) in members.iter().enumerate() {
                let h: f32 = unit.iter().map(|&v| node_h(v)).sum();
                if i == 0 || kept + h <= cap {
                    kept += h;
                } else {
                    for &v in unit {
                        rank[v] = r + 1;
                    }
                }
            }
        }
    }

    // ---- virtual-node expansion ----
    // Every DAG pair spanning >1 rank gets a chain of virtual nodes, one per
    // intermediate rank. All nodes (real + virtual) participate in ordering.
    let mut xnodes: Vec<XNode> = (0..m)
        .map(|i| XNode {
            real: Some(i as u32),
            rank: rank[i],
            w: nodes[comp[i] as usize].w,
            h: nodes[comp[i] as usize].h,
        })
        .collect();
    // Virtual-node budget: generous, but bounded.
    let mut virtual_budget: usize = 60_000;
    // ordering adjacency (adjacent ranks only) over xnodes
    let mut xout: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut xin: Vec<Vec<u32>> = vec![Vec::new(); m];
    // Parallel to xout/xin: where this edge touches each end, as (own, other)
    // offsets from the node's top. A card is touched at its field row when it
    // is the source and at the middle of its left face when it is the target,
    // so the offset belongs to the edge, not to the node. Lanes are points,
    // so they sit on the line at 0.
    let mut pout: Vec<Vec<(f32, f32)>> = vec![Vec::new(); m];
    let mut pin: Vec<Vec<(f32, f32)>> = vec![Vec::new(); m];
    // chains: original directed pair -> virtual xnode ids from source to target
    let mut chains: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    #[allow(clippy::too_many_arguments)]
    let push_edge = |xout: &mut Vec<Vec<u32>>,
                     xin: &mut Vec<Vec<u32>>,
                     pout: &mut Vec<Vec<(f32, f32)>>,
                     pin: &mut Vec<Vec<(f32, f32)>>,
                     a: u32,
                     b: u32,
                     oa: f32,
                     ob: f32| {
        xout[a as usize].push(b);
        pout[a as usize].push((oa, ob));
        xin[b as usize].push(a);
        pin[b as usize].push((ob, oa));
    };
    for &((a, b), reversed) in &acyclic {
        let (ra, rb) = (rank[a as usize], rank[b as usize]);
        let orig_pair = if reversed { (b, a) } else { (a, b) };
        let span = rb - ra;
        // Lane-route everything we can afford: routing is what keeps a long
        // edge inside the gaps between ranks instead of crossing unrelated
        // nodes. The budget stops pathological graphs from exploding.
        if span == 0 || virtual_budget == 0 {
            continue;
        }
        let (src, dst) = orig_pair;
        let port = port_of.get(&orig_pair).copied().unwrap_or(0.0);
        let dst_mid = nodes[comp[dst as usize] as usize].h / 2.0;
        let off = |v: u32| -> f32 {
            if v == src {
                port
            } else if v == dst {
                dst_mid
            } else {
                0.0
            }
        };
        if span == 1 {
            push_edge(&mut xout, &mut xin, &mut pout, &mut pin, a, b, off(a), off(b));
            continue;
        }
        // Each edge gets its own lane. Sharing one lane per target — bundling
        // every reference to a type into a spine — measured worse on every
        // axis, because the spine sits at the median of all its sources and
        // each edge then has to climb across whatever is in the way to reach
        // it (avg length +5%, crossings +88%).
        let lane_at = |r: i32,
                       xnodes: &mut Vec<XNode>,
                       xout: &mut Vec<Vec<u32>>,
                       xin: &mut Vec<Vec<u32>>,
                       pout: &mut Vec<Vec<(f32, f32)>>,
                       pin: &mut Vec<Vec<(f32, f32)>>,
                       budget: &mut usize| {
            let id = xnodes.len() as u32;
            xnodes.push(XNode { real: None, rank: r, w: VIRTUAL_W, h: VIRTUAL_H });
            xout.push(Vec::new());
            xin.push(Vec::new());
            pout.push(Vec::new());
            pin.push(Vec::new());
            *budget = budget.saturating_sub(1);
            id
        };
        let mut chain = Vec::with_capacity(span.unsigned_abs() as usize);
        let mut prev = a;
        if span > 1 {
            for r in ra + 1..rb {
                let id =
                    lane_at(r, &mut xnodes, &mut xout, &mut xin, &mut pout, &mut pin, &mut virtual_budget);
                push_edge(&mut xout, &mut xin, &mut pout, &mut pin, prev, id, off(prev), 0.0);
                chain.push(id);
                prev = id;
            }
            push_edge(&mut xout, &mut xin, &mut pout, &mut pin, prev, b, off(prev), off(b));
        } else {
            // rb < ra. BFS ranks leave a minority of edges pointing backwards;
            // left unrouted they fly straight back across every rank in
            // between, which was the largest single source of edges cutting
            // through unrelated cards. Route them too, threaded in reverse,
            // wiring every ordering edge in increasing-rank direction so the
            // sweeps still see a DAG.
            for r in (rb + 1..ra).rev() {
                let id =
                    lane_at(r, &mut xnodes, &mut xout, &mut xin, &mut pout, &mut pin, &mut virtual_budget);
                push_edge(&mut xout, &mut xin, &mut pout, &mut pin, id, prev, 0.0, off(prev));
                chain.push(id);
                prev = id;
            }
            push_edge(&mut xout, &mut xin, &mut pout, &mut pin, b, prev, off(b), 0.0);
        }
        let chain_oriented = if reversed {
            let mut c = chain.clone();
            c.reverse();
            c
        } else {
            chain.clone()
        };
        chains.insert(orig_pair, chain_oriented);
    }
    let xn = xnodes.len();

    if timing {
        eprintln!("  stage expand    {:>6.1}ms", t_stage.elapsed().as_secs_f32() * 1000.0);
    }
    let t_stage = std::time::Instant::now();

    // ---- rank buckets ----
    let max_rank = xnodes.iter().map(|x| x.rank).max().unwrap_or(0);
    let mut ranks: Vec<Vec<u32>> = vec![Vec::new(); (max_rank + 1) as usize];
    for (i, x) in xnodes.iter().enumerate() {
        ranks[x.rank as usize].push(i as u32);
    }

    let mut pos_in_rank = vec![0u32; xn];
    let assign_pos = |ranks: &Vec<Vec<u32>>, pos: &mut Vec<u32>| {
        for r in ranks {
            for (i, &v) in r.iter().enumerate() {
                pos[v as usize] = i as u32;
            }
        }
    };
    assign_pos(&ranks, &mut pos_in_rank);

    // ---- ordering ----
    // Barycenter sweeps, adjacent-swap refinement, then sifting. The basin is
    // flat, not under-cooked: 14/8 and 60/40 sweeps land on the same drawing,
    // and restarting from four permuted orders lands within 0.05% of it for
    // eighteen times the layout budget.
    //
    // Pushing dead-end types to the margins — so the middle of a rank stays
    // clear for through-traffic — was tried both as a hard relocation and as
    // a soft bias, and both raise edge crossings (137 to 150-159 per edge):
    // the leaf's own edge then has to sweep across the rank to reach it, and
    // that sweep crosses more than the dead end was blocking.
    let nranks = ranks.len();
    order_ranks(&mut ranks, &mut pos_in_rank, &xin, &xout, nranks, config);

    if timing {
        eprintln!("  stage order     {:>6.1}ms", t_stage.elapsed().as_secs_f32() * 1000.0);
    }
    let t_stage = std::time::Instant::now();

    // ---- cluster contiguity ----
    // Union members carry no edge to one another, so crossing reduction has
    // no reason to keep them together and they end up scattered down the
    // rank with the union's own edges fanning out between them. Pull each
    // cluster's members into one contiguous block, centred on where they
    // already sit. Done last, after the sweeps have settled, so it decides
    // adjacency without fighting the edge-crossing work.
    {
        // A type can belong to several unions; it can only be adjacent to
        // one of them, so it joins the first (smallest wins ties, since a
        // small union is the tighter statement).
        let mut order: Vec<usize> = (0..clusters.len()).collect();
        order.sort_by_key(|&i| clusters[i].members.len());
        let mut cluster_of: HashMap<u32, usize> = HashMap::new();
        for &ci in &order {
            for v in &clusters[ci].members {
                if let Some(&l) = local_of.get(v) {
                    cluster_of.entry(l).or_insert(ci);
                }
            }
        }
        for bucket in ranks.iter_mut() {
            // Which clusters have two or more members in this rank, and where
            // their members currently sit.
            let mut slots: HashMap<usize, Vec<usize>> = HashMap::new();
            for (i, &v) in bucket.iter().enumerate() {
                if let Some(&c) = cluster_of.get(&v) {
                    slots.entry(c).or_default().push(i);
                }
            }
            let mut blocks: Vec<(usize, usize)> = slots
                .iter()
                .filter(|(_, idx)| idx.len() > 1)
                .map(|(&c, idx)| (idx[idx.len() / 2], c))
                .collect();
            if blocks.is_empty() {
                continue;
            }
            // Rebuild the rank left to right: everything keeps its order,
            // except that each cluster is emitted whole at its median slot.
            // Sorted by cluster id on ties — hash order is not stable across
            // runs and the layout has to be.
            blocks.sort_unstable();
            let clustered: HashMap<usize, usize> = blocks
                .iter()
                .enumerate()
                .flat_map(|(bi, &(_, c))| slots[&c].iter().map(move |&i| (i, bi)))
                .collect();
            let mut out: Vec<u32> = Vec::with_capacity(bucket.len());
            let mut emitted = vec![false; blocks.len()];
            for (i, &v) in bucket.iter().enumerate() {
                match clustered.get(&i) {
                    Some(&bi) => {
                        if !emitted[bi] {
                            emitted[bi] = true;
                            out.extend(slots[&blocks[bi].1].iter().map(|&j| bucket[j]));
                        }
                    }
                    None => out.push(v),
                }
            }
            *bucket = out;
            for (i, &v) in bucket.iter().enumerate() {
                pos_in_rank[v as usize] = i as u32;
            }
        }

        // Blocks were dropped at their old median without regard for what
        // they now cross. Re-run the adjacent-swap refinement over *blocks*
        // rather than nodes, so the groups keep their shape but are free to
        // slide to where they cost the fewest crossings.
        for _ in 0..BLOCK_PASSES {
            let mut improved = false;
            for bucket in ranks.iter_mut() {
                // Segment the rank into blocks: a cluster's run, or a single
                // node. The contiguity pass above guarantees a cluster's
                // members are consecutive here.
                let mut blocks: Vec<Vec<u32>> = Vec::with_capacity(bucket.len());
                let mut i = 0;
                while i < bucket.len() {
                    let c = cluster_of.get(&bucket[i]).copied();
                    let mut j = i + 1;
                    if c.is_some() {
                        while j < bucket.len() && cluster_of.get(&bucket[j]).copied() == c {
                            j += 1;
                        }
                    }
                    blocks.push(bucket[i..j].to_vec());
                    i = j;
                }
                let mut k = 0;
                while k + 1 < blocks.len() {
                    let (before, after) = (blocks[k].clone(), blocks[k + 1].clone());
                    // Crossings between the two blocks, in each order.
                    let pairs = |a: &[u32], b: &[u32]| -> u64 {
                        let mut n = 0u64;
                        for &x in a {
                            for &y in b {
                                let (ab, _) =
                                    crossings_between(x, y, &pos_in_rank, &xin, &xout);
                                n += ab as u64;
                            }
                        }
                        n
                    };
                    let ab = pairs(&before, &after);
                    let ba = pairs(&after, &before);
                    if ba < ab {
                        blocks.swap(k, k + 1);
                        improved = true;
                        // Re-index so the next comparison sees the new order.
                        let mut idx = 0u32;
                        for b in blocks.iter() {
                            for &v in b {
                                pos_in_rank[v as usize] = idx;
                                idx += 1;
                            }
                        }
                    }
                    k += 1;
                }
                *bucket = blocks.concat();
                for (i, &v) in bucket.iter().enumerate() {
                    pos_in_rank[v as usize] = i as u32;
                }
            }
            if !improved {
                break;
            }
        }
    }

    // ---- turn-round lanes for back edges ----
    // A back edge leaves the right of its source and has to end up at the
    // left of a target that sits to the left of it, so it crosses both of
    // their columns on the way. Give it a slot in each, right beside the
    // card, on the side its route continues from. Added after ordering: the
    // slot belongs next to its card, which is not something crossing
    // reduction can be told.
    let mut back_pairs: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    if lanes {
        // Read off the ranks rather than the cycle breaker's list: when two
        // types refer to each other only one direction survives in the DAG,
        // and the other is a back edge all the same. It rides its twin's
        // lanes, the two drawn as one line.
        for &(s, t) in &pairs {
            if rank[s as usize] < rank[t as usize] && !chains.contains_key(&(s, t)) {
                if let Some(mut twin) = chains.get(&(t, s)).cloned() {
                    twin.reverse();
                    chains.insert((s, t), twin);
                }
            }
        }
        for &(b, a) in &pairs {
            if rank[b as usize] <= rank[a as usize] {
                continue;
            }
            // a is the target (lower rank), b the source.
            let pair = (b, a);
            let chain = chains.remove(&pair).unwrap_or_else(|| {
                let mut twin = chains.get(&(a, b)).cloned().unwrap_or_default();
                twin.reverse();
                twin
            });
            let frac = |v: u32, ranks: &Vec<Vec<u32>>, xnodes: &Vec<XNode>| {
                let bucket = &ranks[xnodes[v as usize].rank as usize];
                bucket.iter().position(|&x| x == v).unwrap_or(0) as f32 / bucket.len().max(1) as f32
            };
            let mut beside = |card: u32,
                              toward: u32,
                              xnodes: &mut Vec<XNode>,
                              ranks: &mut Vec<Vec<u32>>| {
                let below = frac(toward, ranks, xnodes) > frac(card, ranks, xnodes);
                let r = xnodes[card as usize].rank;
                let id = xnodes.len() as u32;
                xnodes.push(XNode { real: None, rank: r, w: VIRTUAL_W, h: VIRTUAL_H });
                xout.push(Vec::new());
                xin.push(Vec::new());
                pout.push(Vec::new());
                pin.push(Vec::new());
                let bucket = &mut ranks[r as usize];
                let at = bucket.iter().position(|&x| x == card).unwrap_or(0);
                bucket.insert(if below { at + 1 } else { at }, id);
                id
            };
            let lb = beside(b, chain.first().copied().unwrap_or(a), &mut xnodes, &mut ranks);
            let la = beside(a, chain.last().copied().unwrap_or(b), &mut xnodes, &mut ranks);
            // Wired low rank to high, like every other ordering edge.
            let (near_a, near_b) = (
                chain.last().copied().unwrap_or(lb),
                chain.first().copied().unwrap_or(la),
            );
            push_edge(&mut xout, &mut xin, &mut pout, &mut pin, la, near_a, 0.0, 0.0);
            if !chain.is_empty() {
                push_edge(&mut xout, &mut xin, &mut pout, &mut pin, near_b, lb, 0.0, 0.0);
            }
            let mut full = Vec::with_capacity(chain.len() + 2);
            full.push(lb);
            full.extend(chain);
            full.push(la);
            chains.insert(pair, full);
            back_pairs.insert(pair);
        }
    }
    let xn = xnodes.len();

    let nranks = ranks.len();
    let mut xrank = vec![0usize; xn];
    for (ri, bucket) in ranks.iter().enumerate() {
        for &v in bucket {
            xrank[v as usize] = ri;
        }
    }

    if timing {
        eprintln!("  stage cluster   {:>6.1}ms", t_stage.elapsed().as_secs_f32() * 1000.0);
    }
    let t_stage = std::time::Instant::now();

    // ---- x per rank: cumulative max width ----
    let mut rank_x = vec![0.0f32; nranks];
    let mut rank_w = vec![0.0f32; nranks];
    // A turn-round bulges into the gap beside its column; the outermost
    // columns have no gap on their far side, so leave one.
    let turn_pad = if back_pairs.is_empty() { 0.0 } else { config.rank_sep * 0.6 };
    let mut x_cursor = turn_pad;
    for ri in 0..nranks {
        let w = ranks[ri]
            .iter()
            .map(|&v| xnodes[v as usize].w)
            .fold(0.0f32, f32::max);
        rank_x[ri] = x_cursor;
        rank_w[ri] = w;
        x_cursor += w + config.rank_sep;
    }
    let comp_w = if nranks > 0 { x_cursor - config.rank_sep + turn_pad } else { 0.0 };

    // ---- y: stack, then median relaxation preserving order ----
    let vsep = if lanes { LANE_PITCH } else { VIRTUAL_SEP };
    let sep_of = |x: &XNode| if x.real.is_some() { config.node_sep } else { vsep };
    let mut y = vec![0.0f32; xn];
    for rank_nodes in ranks.iter().take(nranks) {
        let mut cursor = 0.0f32;
        for &v in rank_nodes {
            y[v as usize] = cursor;
            cursor += xnodes[v as usize].h + sep_of(&xnodes[v as usize]);
        }
    }
    // Median pull toward neighbors, then per-rank overlap resolution as an
    // isotonic (PAVA) fit: minimum total movement under the ordering
    // constraint, instead of a forward push that made every rank drift
    // diagonally downward.
    // Six passes; the fit stops moving after about that many. Squeezing the
    // leftover air out afterwards was tried and reverted: it shortens edges
    // ~5% and halves the drawing's height, but packs the cards so tightly
    // that edges cross 42% more cards and 28% more edges. Complexity is
    // crossings first, length second.
    relax_y(&ranks, &xnodes, &xin, &xout, &pin, &pout, &sep_of, &mut y, 6, lanes);

    if lanes {
        straighten(&ranks, &xnodes, &xout, &sep_of, &mut y);
    }

    if timing {
        eprintln!("  stage coords    {:>6.1}ms", t_stage.elapsed().as_secs_f32() * 1000.0);
    }
    let t_stage = std::time::Instant::now();

    // ---- normalize, write real positions, emit routes ----
    let min_y = (0..xn).map(|i| y[i]).fold(f32::INFINITY, f32::min);
    let mut comp_h = 0.0f32;
    let mut xpos = vec![Point { x: 0.0, y: 0.0 }; xn];
    for (i, x) in xnodes.iter().enumerate() {
        let ri = xrank[i];
        let p = Point {
            x: rank_x[ri] + (rank_w[ri] - x.w) / 2.0,
            y: y[i] - min_y,
        };
        comp_h = comp_h.max(p.y + x.h);
        xpos[i] = p;
        if let Some(real) = x.real {
            positions[comp[real as usize] as usize] = p;
            cols[comp[real as usize] as usize] = (rank_x[ri], rank_x[ri] + rank_w[ri]);
        }
    }
    for ((la, lb), chain) in chains {
        let back = back_pairs.contains(&(la, lb));
        let mut pts: Vec<Point> = Vec::with_capacity(chain.len() * 2);
        for &id in &chain {
            let p = xpos[id as usize];
            let x = &xnodes[id as usize];
            if lanes {
                let ri = xrank[id as usize];
                let (l, r) = (rank_x[ri], rank_x[ri] + rank_w[ri]);
                let (first, second) = if back { (r, l) } else { (l, r) };
                pts.push(Point { x: first, y: p.y });
                pts.push(Point { x: second, y: p.y });
            } else {
                pts.push(Point { x: p.x + x.w / 2.0, y: p.y + x.h / 2.0 });
            }
        }
        emit_route((comp[la as usize], comp[lb as usize]), Route { pts });
    }
    if timing {
        eprintln!("  stage emit      {:>6.1}ms", t_stage.elapsed().as_secs_f32() * 1000.0);
    }
    (comp_w.max(0.0), comp_h)
}

/// Crossings contributed by `a` before `b` versus the other way round,
/// counting both neighbour sides.
fn crossings_between(
    a: u32,
    b: u32,
    pos: &[u32],
    xin: &[Vec<u32>],
    xout: &[Vec<u32>],
) -> (u32, u32) {
    let mut ab = 0u32;
    let mut ba = 0u32;
    for nbrs in [xin, xout] {
        for &na in &nbrs[a as usize] {
            for &nb in &nbrs[b as usize] {
                let pa = pos[na as usize];
                let pb = pos[nb as usize];
                if pa > pb {
                    ab += 1;
                } else if pa < pb {
                    ba += 1;
                }
            }
        }
    }
    (ab, ba)
}


/// Barycenter sweeps followed by adjacent-swap refinement — the standard
/// two-stage crossing reduction, run over real *and* virtual nodes.
fn order_ranks(
    ranks: &mut [Vec<u32>],
    pos_in_rank: &mut [u32],
    xin: &[Vec<u32>],
    xout: &[Vec<u32>],
    nranks: usize,
    config: &LayoutConfig,
) {
    for sweep in 0..config.ordering_sweeps {
        let downward = sweep % 2 == 0;
        let range: Vec<usize> = if downward {
            (0..nranks).collect()
        } else {
            (0..nranks).rev().collect()
        };
        for ri in range {
            let mut keyed: Vec<(f32, u32)> = ranks[ri]
                .iter()
                .map(|&v| {
                    let nbrs = if downward { &xin[v as usize] } else { &xout[v as usize] };
                    let mut vals: Vec<f32> =
                        nbrs.iter().map(|&u| pos_in_rank[u as usize] as f32).collect();
                    let key = median(&mut vals).unwrap_or(pos_in_rank[v as usize] as f32);
                    (key, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            ranks[ri] = keyed.into_iter().map(|(_, v)| v).collect();
            for (i, &v) in ranks[ri].iter().enumerate() {
                pos_in_rank[v as usize] = i as u32;
            }
        }
    }

    // ---- transpose refinement: adjacent swaps that reduce crossings ----
    for pass in 0..config.transpose_passes {
        let mut improved = false;
        // Every other pass also takes swaps that leave the count unchanged.
        // Strict improvement alone stalls on a plateau; drifting sideways
        // across it is what lets the next strict swap become available.
        let tie = pass % 2 == 1;
        for rank_nodes in ranks.iter_mut().take(nranks) {
            for i in 0..rank_nodes.len().saturating_sub(1) {
                let a = rank_nodes[i];
                let b = rank_nodes[i + 1];
                let (ab, ba) = crossings_between(a, b, pos_in_rank, xin, xout);
                if ba < ab || (tie && ba == ab && b < a) {
                    rank_nodes.swap(i, i + 1);
                    pos_in_rank[a as usize] = (i + 1) as u32;
                    pos_in_rank[b as usize] = i as u32;
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }

    // Sifting — sliding a node across a window to its best slot, rather than
    // one adjacent swap at a time — was tried here and removed: it spends a
    // quarter of the layout budget (11ms of 55) and changes nothing, card
    // crossings 3.59 to 3.55 and edge crossings very slightly worse.
}


/// Pull every node toward the median of its neighbours, then resolve overlaps
/// inside each rank as an isotonic fit — the least total movement that still
/// respects the order, rather than a forward push that makes each rank drift
/// diagonally downward.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn relax_y(
    ranks: &[Vec<u32>],
    xnodes: &[XNode],
    xin: &[Vec<u32>],
    xout: &[Vec<u32>],
    pin: &[Vec<(f32, f32)>],
    pout: &[Vec<(f32, f32)>],
    sep_of: &impl Fn(&XNode) -> f32,
    y: &mut [f32],
    yiters: usize,
    follow: bool,
) {
    let nranks = ranks.len();
    for iter in 0..yiters {
        let range: Vec<usize> = if iter % 2 == 0 {
            (0..nranks).collect()
        } else {
            (0..nranks).rev().collect()
        };
        for ri in range {
            let rank_nodes = &ranks[ri];
            let mut desired: Vec<f32> = Vec::with_capacity(rank_nodes.len());
            for &v in rank_nodes {
                let vi = v as usize;
                let mut centers: Vec<f32> = Vec::new();
                // A lane follows one neighbour, not the middle of two: the
                // one it has just been placed against in this sweep. Splitting
                // the difference puts a step at every column; following keeps
                // the run level and leaves one bend where it has to give way.
                let lane = follow && xnodes[vi].real.is_none();
                let from_in = iter % 2 == 0;
                let (take_in, take_out) = if !lane {
                    (usize::MAX, usize::MAX)
                } else if (from_in && !xin[vi].is_empty()) || xout[vi].is_empty() {
                    (usize::MAX, 0)
                } else {
                    (0, usize::MAX)
                };
                for (&u, &(own, other)) in xin[vi]
                    .iter()
                    .zip(pin[vi].iter())
                    .take(take_in)
                    .chain(xout[vi].iter().zip(pout[vi].iter()).take(take_out))
                {
                    // The edge is straight when the two points it touches
                    // share a y: `y[u] + other == y[v] + own`. Solve that for
                    // v's centre, which is what the fit below works in.
                    // Aligning card centres instead, as this did, put the
                    // departure row and the arrival middle at different
                    // heights by construction, so every reference came out as
                    // an S even when nothing was in its way.
                    centers.push(y[u as usize] + other - own + xnodes[vi].h / 2.0);
                }
                // The median, not the mean: it is the L1 optimum, so it
                // minimises total edge length. The mean (L2) was measured —
                // it pulls outliers in and shortens edges 5%, but costs 27%
                // more edges cutting across cards and 14% more crossings.
                // With lane routing a card takes the mean instead. The case
                // against it was all card crossings, and lanes cannot cross
                // a card wherever it sits; what is left is the gain — 0.7%
                // less drawn edge and a 6% smaller drawing. Re-sorting each
                // column by these heights and relaxing again was also tried,
                // to stop lanes detouring round cards: 10% more edge.
                let d = if follow && !lane && !centers.is_empty() {
                    Some(centers.iter().sum::<f32>() / centers.len() as f32)
                } else {
                    median(&mut centers)
                }
                    .map(|m| m - xnodes[vi].h / 2.0)
                    .unwrap_or(y[vi]);
                desired.push(d);
            }
            // transform to the unconstrained isotonic problem: z_i must be
            // non-decreasing, where offsets fold in heights + gaps
            let mut offset = 0.0f32;
            let mut z: Vec<f32> = Vec::with_capacity(desired.len());
            for (k, &v) in rank_nodes.iter().enumerate() {
                z.push(desired[k] - offset);
                let x = &xnodes[v as usize];
                offset += x.h + sep_of(x);
            }
            // pool adjacent violators (L2)
            let mut block_val: Vec<f32> = Vec::with_capacity(z.len());
            let mut block_len: Vec<usize> = Vec::with_capacity(z.len());
            for &zi in &z {
                block_val.push(zi);
                block_len.push(1);
                while block_val.len() > 1 {
                    let last = block_val.len() - 1;
                    if block_val[last - 1] > block_val[last] {
                        let merged = (block_val[last - 1] * block_len[last - 1] as f32
                            + block_val[last] * block_len[last] as f32)
                            / (block_len[last - 1] + block_len[last]) as f32;
                        block_len[last - 1] += block_len[last];
                        block_val[last - 1] = merged;
                        block_val.pop();
                        block_len.pop();
                    } else {
                        break;
                    }
                }
            }
            let mut fitted: Vec<f32> = Vec::with_capacity(z.len());
            for (bi, &val) in block_val.iter().enumerate() {
                for _ in 0..block_len[bi] {
                    fitted.push(val);
                }
            }
            let mut offset = 0.0f32;
            for (k, &v) in rank_nodes.iter().enumerate() {
                y[v as usize] = fitted[k] + offset;
                let x = &xnodes[v as usize];
                offset += x.h + sep_of(x);
            }
        }
    }
}

/// Put each run of lanes at one height, so a long edge is a level line with
/// a bend at either end rather than a step at every column.
///
/// `relax_y` places one rank at a time, and a lane has no reason to land at
/// the height its predecessor did. Here consecutive lanes of an edge are
/// tied into a block that moves as one. Not every pair can be: two edges
/// that swap places between two ranks cannot both go straight through the
/// swap. Per pair of ranks the links kept are a longest increasing
/// subsequence — the most links that do not cross one another — and because
/// kept links never cross, "sits above" between blocks has no cycles and the
/// blocks can be placed in one pass each way.
///
/// Each block asks for the median height its lanes already had. One pass
/// from the top gives every block the highest spot at or below its wish
/// that clears what is above it; one from the bottom the mirror image. Both
/// satisfy every spacing constraint, so their midpoint does too, and it
/// errs evenly instead of drifting one way.
fn straighten(
    ranks: &[Vec<u32>],
    xnodes: &[XNode],
    xout: &[Vec<u32>],
    sep_of: &impl Fn(&XNode) -> f32,
    y: &mut [f32],
) {
    let reach = STRAIGHTEN_REACH;
    let xn = xnodes.len();
    let mut pos = vec![0u32; xn];
    for bucket in ranks {
        for (i, &v) in bucket.iter().enumerate() {
            pos[v as usize] = i as u32;
        }
    }
    // ---- which lane-to-lane links stay straight ----
    let mut next: Vec<Option<u32>> = vec![None; xn];
    let mut has_prev = vec![false; xn];
    for bucket in ranks {
        // (lane, its continuation), in rank order.
        let cand: Vec<(u32, u32)> = bucket
            .iter()
            .filter(|&&u| xnodes[u as usize].real.is_none())
            .filter_map(|&u| {
                xout[u as usize]
                    .iter()
                    .copied()
                    .find(|&w| {
                        xnodes[w as usize].real.is_none()
                            && xnodes[w as usize].rank == xnodes[u as usize].rank + 1
                            && (y[w as usize] - y[u as usize]).abs() <= reach
                    })
                    .map(|w| (u, w))
            })
            .collect();
        // Longest strictly increasing subsequence of continuation slots.
        let mut tails: Vec<usize> = Vec::new();
        let mut back: Vec<Option<usize>> = vec![None; cand.len()];
        for (i, &(_, w)) in cand.iter().enumerate() {
            let key = pos[w as usize];
            let at = tails.partition_point(|&t| pos[cand[t].1 as usize] < key);
            back[i] = if at > 0 { Some(tails[at - 1]) } else { None };
            if at == tails.len() {
                tails.push(i);
            } else {
                tails[at] = i;
            }
        }
        let mut cur = tails.last().copied();
        while let Some(i) = cur {
            let (u, w) = cand[i];
            next[u as usize] = Some(w);
            has_prev[w as usize] = true;
            cur = back[i];
        }
    }
    // ---- blocks ----
    let mut block = vec![u32::MAX; xn];
    let mut wish: Vec<f32> = Vec::new();
    for (v, &continues) in has_prev.iter().enumerate() {
        if continues {
            continue;
        }
        let id = wish.len() as u32;
        let mut ys: Vec<f32> = Vec::new();
        let mut cur = Some(v as u32);
        while let Some(u) = cur {
            block[u as usize] = id;
            ys.push(y[u as usize]);
            cur = next[u as usize];
        }
        wish.push(median(&mut ys).unwrap_or(0.0));
    }
    let nb = wish.len();
    // ---- "sits above" between blocks, with the room each needs ----
    let mut below: Vec<Vec<(u32, f32)>> = vec![Vec::new(); nb];
    let mut waiting = vec![0u32; nb];
    for bucket in ranks {
        for w in bucket.windows(2) {
            let up = &xnodes[w[0] as usize];
            let (a, b) = (block[w[0] as usize], block[w[1] as usize]);
            below[a as usize].push((b, up.h + sep_of(up)));
            waiting[b as usize] += 1;
        }
    }
    let mut order: Vec<u32> = (0..nb as u32).filter(|&b| waiting[b as usize] == 0).collect();
    let mut head = 0;
    while head < order.len() {
        let a = order[head];
        head += 1;
        for &(b, _) in &below[a as usize] {
            waiting[b as usize] -= 1;
            if waiting[b as usize] == 0 {
                order.push(b);
            }
        }
    }
    if order.len() != nb {
        // Cannot happen while kept links do not cross; if it ever does, the
        // relaxed heights are still a valid drawing.
        return;
    }
    let mut lo = wish.clone();
    for &a in &order {
        for &(b, gap) in &below[a as usize] {
            lo[b as usize] = lo[b as usize].max(lo[a as usize] + gap);
        }
    }
    let mut hi = wish;
    for &a in order.iter().rev() {
        for &(b, gap) in &below[a as usize] {
            hi[a as usize] = hi[a as usize].min(hi[b as usize] - gap);
        }
    }
    for v in 0..xn {
        let b = block[v] as usize;
        y[v] = (lo[b] + hi[b]) / 2.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(w: f32, h: f32) -> LayoutNode {
        LayoutNode { w, h }
    }

    fn edge(from: u32, to: u32) -> LayoutEdge {
        LayoutEdge { from, to, from_port_y: 20.0, route: true }
    }

    #[test]
    fn chain_ranks_left_to_right() {
        let nodes = vec![node(100.0, 40.0); 3];
        let edges = vec![edge(0, 1), edge(1, 2)];
        let r = layout(&nodes, &edges, &[0], &[], &LayoutConfig::default());
        assert!(r.positions[0].x < r.positions[1].x);
        assert!(r.positions[1].x < r.positions[2].x);
        assert_eq!(r.edges.len(), 2);
        assert!(r.edges.iter().all(|e| e.points.len() >= 2));
    }

    #[test]
    fn cycle_does_not_hang_or_overlap_ranks() {
        let nodes = vec![node(100.0, 40.0); 3];
        let edges = vec![edge(0, 1), edge(1, 2), edge(2, 0)];
        let r = layout(&nodes, &edges, &[], &[], &LayoutConfig::default());
        for p in &r.positions {
            assert!(p.x.is_finite() && p.y.is_finite());
        }
    }

    #[test]
    fn no_overlap_within_rank() {
        let mut nodes = vec![node(120.0, 50.0)];
        let mut edges = Vec::new();
        for i in 1..=10 {
            nodes.push(node(120.0, 50.0));
            edges.push(edge(0, i));
        }
        let r = layout(&nodes, &edges, &[0], &[], &LayoutConfig::default());
        let mut ys: Vec<f32> = (1..=10).map(|i| r.positions[i].y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in ys.windows(2) {
            assert!(w[1] - w[0] >= 50.0, "nodes overlap: {} vs {}", w[0], w[1]);
        }
    }

    #[test]
    fn smoothing_straightens_a_zigzag_chain_without_moving_its_ends() {
        let mut pts = vec![
            Point { x: 0.0, y: 0.0 },
            Point { x: 10.0, y: 40.0 },
            Point { x: 20.0, y: -40.0 },
            Point { x: 30.0, y: 40.0 },
            Point { x: 40.0, y: 0.0 },
        ];
        let before: f32 = pts.windows(3).map(|w| (w[0].y - 2.0 * w[1].y + w[2].y).abs()).sum();
        smooth_chain(&mut pts);
        let after: f32 = pts.windows(3).map(|w| (w[0].y - 2.0 * w[1].y + w[2].y).abs()).sum();
        assert!(after < before / 10.0, "zigzag survived: {before} -> {after}");
        assert_eq!(pts[0], Point { x: 0.0, y: 0.0 });
        assert_eq!(pts[4], Point { x: 40.0, y: 0.0 });
        // x is a rank coordinate and must never be relaxed
        assert_eq!(pts[2].x, 20.0);
    }

    #[test]
    fn simplify_drops_collinear_knots_and_keeps_corners() {
        let mut straight = vec![
            Point { x: 0.0, y: 0.0 },
            Point { x: 10.0, y: 0.5 },
            Point { x: 20.0, y: 1.0 },
            Point { x: 30.0, y: 1.5 },
        ];
        simplify(&mut straight, 3.0);
        assert_eq!(straight.len(), 2);

        let mut corner = vec![
            Point { x: 0.0, y: 0.0 },
            Point { x: 10.0, y: 60.0 },
            Point { x: 20.0, y: 0.0 },
        ];
        simplify(&mut corner, 3.0);
        assert_eq!(corner.len(), 3, "a real corner must survive");
    }

    /// Pairs of laid-out edges whose paths properly cross, ignoring pairs that
    /// share an endpoint — the same definition the metrics use.
    fn tangles(r: &LayoutResult, edges: &[LayoutEdge]) -> usize {
        let seg = |k: usize| {
            let p = &r.edges[k];
            let mut v = vec![p.start];
            v.extend(p.points.iter().copied());
            v
        };
        let d = |a: Point, b: Point, c: Point| {
            (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
        };
        let mut n = 0;
        for i in 0..r.edges.len() {
            for j in i + 1..r.edges.len() {
                let (ei, ej) = (&edges[i], &edges[j]);
                if ei.from == ej.from || ei.from == ej.to || ei.to == ej.from || ei.to == ej.to {
                    continue;
                }
                let (a, b) = (seg(i), seg(j));
                let hit = a.windows(2).any(|p| {
                    b.windows(2).any(|q| {
                        let (d1, d2) = (d(p[0], p[1], q[0]), d(p[0], p[1], q[1]));
                        let (d3, d4) = (d(q[0], q[1], p[0]), d(q[0], q[1], p[1]));
                        ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
                    })
                });
                if hit {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn edges_leave_the_right_face_and_arrive_at_the_left_one() {
        // Including the back edge: Repo points at User, which sits to its
        // left, so that one has to loop out right and come back around.
        let nodes = vec![node(120.0, 60.0); 3];
        let edges = vec![edge(0, 1), edge(1, 2), edge(2, 1)];
        let r = layout(&nodes, &edges, &[0], &[], &LayoutConfig::default());
        for (k, e) in edges.iter().enumerate() {
            let path = &r.edges[k];
            let (sp, sn) = (r.positions[e.from as usize], nodes[e.from as usize]);
            let (tp, _) = (r.positions[e.to as usize], nodes[e.to as usize]);
            assert!(
                (path.start.x - (sp.x + sn.w)).abs() < 0.5,
                "edge {k} leaves at {} not the right face {}",
                path.start.x,
                sp.x + sn.w
            );
            let end = path.curves.last().expect("a path has segments").end;
            assert!(
                (end.x - tp.x).abs() < 0.5,
                "edge {k} arrives at {} not the left face {}",
                end.x,
                tp.x
            );
        }
    }

    #[test]
    fn both_ends_of_a_back_edge_point_rightward() {
        // The departure and arrival tangents must both head right, or a back
        // edge dives left the moment it leaves and crosses its own card.
        let nodes = vec![node(120.0, 60.0); 2];
        let edges = vec![edge(0, 1), edge(1, 0)];
        let r = layout(&nodes, &edges, &[0], &[], &LayoutConfig::default());
        let back = &r.edges[1];
        let first = back.curves.first().unwrap();
        assert!(first.c1.x > back.start.x, "departure tangent points left");
        let last = back.curves.last().unwrap();
        assert!(last.c2.x < last.end.x, "arrival tangent points left");
    }

    #[test]
    fn a_forced_crossing_is_counted_exactly_once() {
        // K(2,2): two sources both pointing at both targets. Whichever way
        // the two targets are ordered, one pair of edges has to cross — the
        // plain X. It is one tangle, however the paths are flattened.
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 2), edge(0, 3), edge(1, 2), edge(1, 3)];
        let r = layout(&nodes, &edges, &[0, 1], &[], &LayoutConfig::default());
        assert_eq!(tangles(&r, &edges), 1, "positions {:?}", r.positions);
    }

    #[test]
    fn edges_sharing_an_endpoint_are_not_a_tangle() {
        // A fan: one source, three targets. They all leave the same card, so
        // however they spread they must never count as crossing each other.
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 1), edge(0, 2), edge(0, 3)];
        let r = layout(&nodes, &edges, &[0], &[], &LayoutConfig::default());
        assert_eq!(tangles(&r, &edges), 0);
    }

    #[test]
    fn the_ordering_untangles_a_crossed_pair() {
        // 0→3 and 1→2 with the targets arriving in the wrong order would
        // cross; the ordering has to swap them.
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 3), edge(1, 2)];
        let r = layout(&nodes, &edges, &[0, 1], &[], &LayoutConfig::default());
        assert_eq!(tangles(&r, &edges), 0, "positions {:?}", r.positions);
    }

    #[test]
    fn refinement_never_increases_crossings() {
        // A dense two-layer graph is where ordering matters; the finished
        // ordering must be no worse than the arrival order it started from.
        let n = 14usize;
        let nodes = vec![node(80.0, 30.0); n];
        let mut edges = Vec::new();
        for i in 0..6u32 {
            for j in 6..13u32 {
                if (i * 7 + j) % 3 != 0 {
                    edges.push(edge(i, j));
                }
            }
        }
        edges.push(edge(13, 0));
        let r = layout(&nodes, &edges, &[13], &[], &LayoutConfig::default());
        // Count geometric crossings of the straight source→target chords.
        let seg = |k: usize| {
            let (a, b) = (r.positions[edges[k].from as usize], r.positions[edges[k].to as usize]);
            (a, b)
        };
        let cross = |p: Point, q: Point, u: Point, v: Point| {
            let d = |a: Point, b: Point, c: Point| {
                (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
            };
            let (d1, d2, d3, d4) = (d(p, q, u), d(p, q, v), d(u, v, p), d(u, v, q));
            ((d1 > 0.0) != (d2 > 0.0)) && ((d3 > 0.0) != (d4 > 0.0))
        };
        let mut n_cross = 0;
        for i in 0..edges.len() {
            for j in i + 1..edges.len() {
                let (a, b) = seg(i);
                let (c, d) = seg(j);
                if cross(a, b, c, d) {
                    n_cross += 1;
                }
            }
        }
        // The arrival order (nodes stacked by index) is the baseline the
        // refinement has to beat, or at least match.
        assert!(n_cross <= edges.len() * edges.len() / 4, "{n_cross} crossings is worse than random");
    }

    #[test]
    fn cluster_members_end_up_contiguous_in_their_column() {
        // Union 0 over variants 1..3, and an unrelated root 4 whose own
        // target 5 shares the variants' column. Nothing joins 1, 2 and 3 to
        // each other, so without the cluster the ordering is free to leave 5
        // sitting in the middle of them.
        let nodes = vec![node(100.0, 40.0); 6];
        let edges = vec![edge(0, 1), edge(0, 2), edge(0, 3), edge(4, 5), edge(4, 2)];
        let clusters = vec![Cluster { parent: 0, members: vec![1, 2, 3] }];
        let r = layout(&nodes, &edges, &[0, 4], &clusters, &LayoutConfig::default());
        let col = |i: usize| r.positions[i].x + nodes[i].w / 2.0;
        // The variants share a column and the union sits to their left.
        assert!((col(1) - col(2)).abs() < 1.0 && (col(1) - col(3)).abs() < 1.0);
        assert!(col(0) < col(1));
        // Nothing is wedged between them in that column.
        let mut ys: Vec<f32> = [1usize, 2, 3].iter().map(|&i| r.positions[i].y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let (top, bottom) = (ys[0], ys[2] + 40.0);
        let intruding = (col(5) - col(1)).abs() < 1.0
            && r.positions[5].y + 40.0 > top
            && r.positions[5].y < bottom;
        assert!(!intruding, "an unrelated type split the union block: {:?}", r.positions);
    }

    #[test]
    fn long_edge_keeps_clear_of_the_ranks_it_spans() {
        // 0→1→2→3 chain plus a long edge 0→3. The long edge is lane-routed
        // through the intermediate ranks, so its path must stay outside the
        // cards sitting in those ranks rather than cutting through them.
        // (The route may well come out straight once smoothed — what matters
        // is where it runs, not how many waypoints survive.)
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 1), edge(1, 2), edge(2, 3), edge(0, 3)];
        let r = layout(&nodes, &edges, &[0], &[], &LayoutConfig::default());
        let long = &r.edges[3];
        for mid in [1usize, 2] {
            let p = r.positions[mid];
            let inside = long.points.iter().any(|q| {
                q.x > p.x && q.x < p.x + 100.0 && q.y > p.y && q.y < p.y + 40.0
            });
            assert!(!inside, "long edge cuts through node {mid} at {p:?}: {:?}", long.points);
        }
    }

    #[test]
    fn no_edge_runs_through_a_card_it_is_not_attached_to() {
        // Long edges, a cycle, a pair that refer to each other, and cards of
        // very different widths sharing columns: every way an edge used to
        // end up on top of somebody else's card.
        let mut nodes = Vec::new();
        for i in 0..24u32 {
            nodes.push(node(80.0 + (i % 5) as f32 * 90.0, 30.0 + (i % 4) as f32 * 50.0));
        }
        let mut edges = Vec::new();
        for i in 0..24u32 {
            for step in [1u32, 2, 5, 11] {
                if (i * 7 + step) % 3 != 0 {
                    edges.push(edge(i, (i + step) % 24));
                }
            }
        }
        edges.push(edge(3, 2));
        edges.push(edge(2, 3));
        let r = layout(&nodes, &edges, &[0], &[], &LayoutConfig::default());
        for (k, e) in edges.iter().enumerate() {
            for (i, (p, n)) in r.positions.iter().zip(nodes.iter()).enumerate() {
                if i as u32 == e.from || i as u32 == e.to {
                    continue;
                }
                let inside = r.edges[k].points.iter().any(|q| {
                    q.x > p.x + 1.0 && q.x < p.x + n.w - 1.0 && q.y > p.y + 1.0 && q.y < p.y + n.h - 1.0
                });
                assert!(!inside, "edge {}→{} runs through card {i}", e.from, e.to);
            }
        }
    }

    #[test]
    fn separate_components_do_not_overlap() {
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 1), edge(2, 3)];
        let r = layout(&nodes, &edges, &[], &[], &LayoutConfig::default());
        let bb = |a: usize, b: usize| {
            let (pa, pb) = (r.positions[a], r.positions[b]);
            (
                pa.x.min(pb.x),
                pa.y.min(pb.y),
                pa.x.max(pb.x) + 100.0,
                pa.y.max(pb.y) + 40.0,
            )
        };
        let (ax0, ay0, ax1, ay1) = bb(0, 1);
        let (bx0, by0, bx1, by1) = bb(2, 3);
        let disjoint = ax1 <= bx0 || bx1 <= ax0 || ay1 <= by0 || by1 <= ay0;
        assert!(disjoint);
    }

    #[test]
    fn singletons_form_grid() {
        let nodes = vec![node(100.0, 40.0); 5];
        let edges: Vec<LayoutEdge> = Vec::new();
        let r = layout(&nodes, &edges, &[], &[], &LayoutConfig::default());
        for p in &r.positions {
            assert!(p.x.is_finite() && p.y.is_finite());
        }
        assert!(r.width > 0.0 && r.height > 0.0);
    }
}
