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
//! 3. edges become smooth curves routed **through their virtual-node
//!    waypoints** (a Catmull-Rom spline), so long edges follow the lanes the
//!    ordering carved out instead of slicing across the whole picture; the
//!    waypoint chain is relaxed and simplified first, which both straightens
//!    the route and cuts what the renderer has to flatten each frame
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
    /// Lay the graph out so every edge runs left to right — laminar flow, at
    /// the cost of depth. Off by default: on a schema with real cycles
    /// (User → Repository → owner → User) a monotone layering is 72 columns
    /// wide here instead of 37, and the extra width costs 79% average edge
    /// length and 49% more edges cutting across cards, for backflow down
    /// from 2371 edges to 449.
    pub monotone: bool,
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
            monotone: false,
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
/// Sifting refinement: how many passes, and how far a node may slide. A wider
/// window keeps buying card crossings (3.65/edge at 160) but starts costing
/// edge crossings and length, and edge crossings are the thing to minimise.
const SIFT_PASSES: usize = 4;
const SIFT_WINDOW: usize = 24;

const VIRTUAL_W: f32 = 8.0;
/// Virtual nodes are lanes, not boxes: giving them height would inflate every
/// rank they pass through (thousands of them on a dense schema).
const VIRTUAL_H: f32 = 0.0;
/// Vertical gap between two lanes sharing a rank.
const VIRTUAL_SEP: f32 = 2.0;

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
    let mut routes: HashMap<(u32, u32), Vec<Point>> = HashMap::new();
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
            |pair, waypoints| {
                routes.insert(pair, waypoints);
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
        }
        packed_w = packed_w.max(ox + c.w);
    }
    for (pair, waypoints) in routes.iter_mut() {
        let (ox, oy) = comp_offset[route_comp[pair]];
        for p in waypoints.iter_mut() {
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
            x += nd.w + config.node_sep;
            row_h = row_h.max(nd.h);
            packed_w = packed_w.max(x);
        }
        packed_h = y + row_h;
    }

    // ---- edge paths: port → waypoints → target, Catmull-Rom smoothed ----
    let mut edge_paths = Vec::with_capacity(edges.len());
    for (ei, e) in edges.iter().enumerate() {
        let sp = positions[e.from as usize];
        let sn = nodes[e.from as usize];
        let tp = positions[e.to as usize];
        let tn = nodes[e.to as usize];
        let port_y = e.from_port_y.clamp(8.0, (sn.h - 8.0).max(8.0));
        let waypoints = routes.get(&(e.from, e.to)).map(|v| v.as_slice()).unwrap_or(&[]);
        let (start, end) = anchor_points(sp, sn, tp, tn, port_y, waypoints);
        let mut knots: Vec<Point> = Vec::with_capacity(waypoints.len() + 2);
        knots.push(start);
        knots.extend_from_slice(waypoints);
        knots.push(end);
        smooth_chain(&mut knots);
        simplify(&mut knots, 3.0);
        let curves = spline(&knots);
        edge_paths.push(EdgePath {
            edge_index: ei as u32,
            start,
            points: flatten(start, &curves),
            curves,
        });
    }

    if std::env::var("GOMPASS_PERF").is_ok() {
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
        let n_edges = edges.len().max(1) as f32;
        eprintln!(
            "layout: {} nodes, {} edges, routed {}, avg len {:.0} (dx {:.0} dy {:.0}), longest {:.0}, backward {}, crossings {} ({:.2}/edge), world {:.0}x{:.0}, tangles {} ({:.1} per edge), same-col {}, union gap {:.0} col {}/{} tight {}/{}",
            nodes.len(),
            edges.len(),
            routed,
            total_len / n_edges,
            total_dx / n_edges,
            total_dy / n_edges,
            longest,
            backward,
            crossed,
            crossed as f32 / n_edges,
            packed_w,
            packed_h,
            edge_x,
            2.0 * edge_x as f32 / n_edges,
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
    let first_x = waypoints.first().map(|p| p.x).unwrap_or(tp.x + tn.w / 2.0);
    let last_x = waypoints.last().map(|p| p.x).unwrap_or(sp.x + sn.w / 2.0);
    let s_cx = sp.x + sn.w / 2.0;
    let t_cx = tp.x + tn.w / 2.0;
    let start = Point {
        x: if first_x >= s_cx { sp.x + sn.w } else { sp.x },
        y: sp.y + port_y,
    };
    let end = Point {
        x: if last_x <= t_cx { tp.x } else { tp.x + tn.w },
        y: tp.y + tn.h / 2.0,
    };
    (start, end)
}

/// Distance from the roots, the shallow-but-infeasible layering. Kept as a
/// fallback: it halves the depth of a deep schema, at the price of leaving
/// edges that point backwards.
fn bfs_rank(
    m: usize,
    succ: &[Vec<u32>],
    pred: &[Vec<u32>],
    comp: &[u32],
    is_root: &[bool],
) -> Vec<i32> {
    let mut rank = vec![-1i32; m];
    let mut queue: std::collections::VecDeque<u32> = (0..m as u32)
        .filter(|&v| is_root[comp[v as usize] as usize] || pred[v as usize].is_empty())
        .collect();
    for &v in &queue {
        rank[v as usize] = 0;
    }
    for seed in 0..m {
        if rank[seed] < 0 && queue.is_empty() {
            rank[seed] = 0;
            queue.push_back(seed as u32);
        }
        while let Some(v) = queue.pop_front() {
            let d = rank[v as usize];
            for &w in &succ[v as usize] {
                if rank[w as usize] < 0 {
                    rank[w as usize] = d + 1;
                    queue.push_back(w);
                }
            }
        }
    }
    for r in rank.iter_mut() {
        if *r < 0 {
            *r = 0;
        }
    }
    rank
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
        let h = (dx / 2.0).clamp(24.0, 160.0) * if p2.x >= p1.x { 1.0 } else { -1.0 };
        let c1 = if i == 0 {
            Point { x: p1.x + h, y: p1.y }
        } else {
            let p0 = knots[i - 1];
            Point { x: p1.x + (p2.x - p0.x) / 6.0, y: p1.y + (p2.y - p0.y) / 6.0 }
        };
        let c2 = if i + 2 < n {
            let p3 = knots[i + 2];
            Point { x: p2.x - (p3.x - p1.x) / 6.0, y: p2.y - (p3.y - p1.y) / 6.0 }
        } else {
            Point { x: p2.x - h, y: p2.y }
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
    mut emit_route: impl FnMut((u32, u32), Vec<Point>),
) -> (f32, f32) {
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

    // ---- cycle break: DFS, reverse back edges ----
    // acyclic keeps the pair's ORIGINAL orientation alongside the DAG one.
    let mut acyclic: Vec<((u32, u32), bool)> = Vec::with_capacity(pairs.len());
    {
        let mut fwd: Vec<Vec<u32>> = vec![Vec::new(); m];
        for &(a, b) in &pairs {
            fwd[a as usize].push(b);
        }
        #[derive(Clone, Copy, PartialEq)]
        enum Mark {
            White,
            Gray,
            Black,
        }
        let mut mark = vec![Mark::White; m];
        let mut starts: Vec<u32> = (0..m as u32).collect();
        starts.sort_by_key(|&v| {
            let gid = comp[v as usize] as usize;
            let root = is_root[gid];
            let has_in = pairs.iter().any(|&(_, b)| b == v);
            (!root as u8, has_in as u8)
        });
        let mut stack: Vec<(u32, usize)> = Vec::new();
        for &s in &starts {
            if mark[s as usize] != Mark::White {
                continue;
            }
            mark[s as usize] = Mark::Gray;
            stack.push((s, 0));
            while let Some(&mut (v, ref mut i)) = stack.last_mut() {
                if *i < fwd[v as usize].len() {
                    let w = fwd[v as usize][*i];
                    *i += 1;
                    match mark[w as usize] {
                        Mark::White => {
                            acyclic.push(((v, w), false));
                            mark[w as usize] = Mark::Gray;
                            stack.push((w, 0));
                        }
                        Mark::Gray => acyclic.push(((w, v), true)), // reversed
                        Mark::Black => acyclic.push(((v, w), false)),
                    }
                } else {
                    mark[v as usize] = Mark::Black;
                    stack.pop();
                }
            }
        }
    }
    acyclic.sort_unstable_by_key(|&(p, _)| p);
    acyclic.dedup_by_key(|&mut (p, _)| p);

    // ---- ranking ----
    // Every node's column is its rank, so the sum of edge spans *is* the
    // horizontal half of total edge length — three quarters of it, measured.
    // `ranking::network_simplex` minimises that sum exactly, and is available
    // behind GOMPASS_SIMPLEX, but it is not the default: minimal *rank span*
    // and minimal *pixel length* turn out to be different objectives once a
    // rank's height is bounded. Requiring every edge to advance a column
    // makes this schema 72 columns wide (avg length 8256 against BFS's 4600);
    // relaxing non-tree edges to minlen 0 cuts the span from 24888 to 8785,
    // but then whole ranks overflow and the height balancing spends the
    // saving back. BFS depth is infeasible — a third of the edges come out
    // backwards, and those are routed as such — but it is measurably the
    // shorter drawing.
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut pred: Vec<Vec<u32>> = vec![Vec::new(); m];
    for &((a, b), _) in &acyclic {
        if a != b {
            succ[a as usize].push(b);
            pred[b as usize].push(a);
        }
    }
    let mut rank = if !config.monotone {
        bfs_rank(m, &succ, &pred, comp, is_root)
    } else {
        let redges: Vec<crate::ranking::RankEdge> = acyclic
            .iter()
            .filter(|((a, b), _)| a != b)
            .map(|&((a, b), _)| crate::ranking::RankEdge {
                from: a,
                to: b,
                weight: 1,
                minlen: 1,
            })
            .collect();
        crate::ranking::network_simplex(m, &redges, SIMPLEX_PIVOTS)
    };

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
    // chains: original directed pair -> virtual xnode ids from source to target
    let mut chains: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    let push_edge = |xout: &mut Vec<Vec<u32>>, xin: &mut Vec<Vec<u32>>, a: u32, b: u32| {
        xout[a as usize].push(b);
        xin[b as usize].push(a);
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
        if span == 1 {
            push_edge(&mut xout, &mut xin, a, b);
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
                       budget: &mut usize| {
            let id = xnodes.len() as u32;
            xnodes.push(XNode { real: None, rank: r, w: VIRTUAL_W, h: VIRTUAL_H });
            xout.push(Vec::new());
            xin.push(Vec::new());
            *budget = budget.saturating_sub(1);
            id
        };
        let mut chain = Vec::with_capacity(span.unsigned_abs() as usize);
        let mut prev = a;
        if span > 1 {
            for r in ra + 1..rb {
                let id = lane_at(r, &mut xnodes, &mut xout, &mut xin, &mut virtual_budget);
                push_edge(&mut xout, &mut xin, prev, id);
                chain.push(id);
                prev = id;
            }
            push_edge(&mut xout, &mut xin, prev, b);
        } else {
            // rb < ra. BFS ranks leave a minority of edges pointing backwards;
            // left unrouted they fly straight back across every rank in
            // between, which was the largest single source of edges cutting
            // through unrelated cards. Route them too, threaded in reverse,
            // wiring every ordering edge in increasing-rank direction so the
            // sweeps still see a DAG.
            for r in (rb + 1..ra).rev() {
                let id = lane_at(r, &mut xnodes, &mut xout, &mut xin, &mut virtual_budget);
                push_edge(&mut xout, &mut xin, id, prev);
                chain.push(id);
                prev = id;
            }
            push_edge(&mut xout, &mut xin, b, prev);
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
        for _ in 0..config.transpose_passes {
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

    let nranks = ranks.len();
    let mut xrank = vec![0usize; xn];
    for (ri, bucket) in ranks.iter().enumerate() {
        for &v in bucket {
            xrank[v as usize] = ri;
        }
    }

    // ---- x per rank: cumulative max width ----
    let mut rank_x = vec![0.0f32; nranks];
    let mut rank_w = vec![0.0f32; nranks];
    let mut x_cursor = 0.0f32;
    for ri in 0..nranks {
        let w = ranks[ri]
            .iter()
            .map(|&v| xnodes[v as usize].w)
            .fold(0.0f32, f32::max);
        rank_x[ri] = x_cursor;
        rank_w[ri] = w;
        x_cursor += w + config.rank_sep;
    }
    let comp_w = if nranks > 0 { x_cursor - config.rank_sep } else { 0.0 };

    // ---- y: stack, then median relaxation preserving order ----
    let sep_of = |x: &XNode| if x.real.is_some() { config.node_sep } else { VIRTUAL_SEP };
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
    relax_y(&ranks, &xnodes, &xin, &xout, &sep_of, &mut y, 6);

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
        }
    }
    for ((la, lb), chain) in chains {
        let waypoints: Vec<Point> = chain
            .iter()
            .map(|&id| {
                let p = xpos[id as usize];
                let x = &xnodes[id as usize];
                Point { x: p.x + x.w / 2.0, y: p.y + x.h / 2.0 }
            })
            .collect();
        emit_route((comp[la as usize], comp[lb as usize]), waypoints);
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

    // Sifting: adjacent swaps can only walk downhill one step at a time, so a
    // node that belongs thirty slots away never gets there. Sifting slides one
    // node across a window, tracking the running crossing delta, and drops it
    // at the best position it saw. Restricted to a window because the cost is
    // the window times the node's degree, and the gain is all in the first
    // few dozen slots.
    for _ in 0..SIFT_PASSES {
        let mut improved = false;
        for rank_nodes in ranks.iter_mut().take(nranks) {
            let len = rank_nodes.len();
            for start in 0..len {
                let v = rank_nodes[start];
                let lo = start.saturating_sub(SIFT_WINDOW);
                let hi = (start + SIFT_WINDOW).min(len - 1);
                // Walk left from `start`, then right, accumulating the change
                // in crossings against each node passed.
                let (mut best_delta, mut best_at) = (0i64, start);
                let mut delta = 0i64;
                for i in (lo..start).rev() {
                    let u = rank_nodes[i];
                    let (uv, vu) = crossings_between(u, v, pos_in_rank, xin, xout);
                    delta += vu as i64 - uv as i64;
                    if delta < best_delta {
                        best_delta = delta;
                        best_at = i;
                    }
                }
                delta = 0;
                for (i, &u) in rank_nodes.iter().enumerate().take(hi + 1).skip(start + 1) {
                    let (vu, uv) = crossings_between(v, u, pos_in_rank, xin, xout);
                    delta += uv as i64 - vu as i64;
                    if delta < best_delta {
                        best_delta = delta;
                        best_at = i;
                    }
                }
                if best_delta < 0 && best_at != start {
                    let v = rank_nodes.remove(start);
                    rank_nodes.insert(best_at, v);
                    let (a, b) = (start.min(best_at), start.max(best_at));
                    for (i, &w) in rank_nodes.iter().enumerate().take(b + 1).skip(a) {
                        pos_in_rank[w as usize] = i as u32;
                    }
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
}


/// Pull every node toward the median of its neighbours, then resolve overlaps
/// inside each rank as an isotonic fit — the least total movement that still
/// respects the order, rather than a forward push that makes each rank drift
/// diagonally downward.
#[allow(clippy::too_many_arguments)]
fn relax_y(
    ranks: &[Vec<u32>],
    xnodes: &[XNode],
    xin: &[Vec<u32>],
    xout: &[Vec<u32>],
    sep_of: &impl Fn(&XNode) -> f32,
    y: &mut [f32],
    yiters: usize,
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
                for &u in xin[vi].iter().chain(xout[vi].iter()) {
                    let ui = u as usize;
                    centers.push(y[ui] + xnodes[ui].h / 2.0);
                }
                let d = median(&mut centers)
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
