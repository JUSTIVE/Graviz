//! Layered (Sugiyama-style) graph layout, left-to-right.
//!
//! Replaces the web app's GraphViz-WASM `dot` pipeline (and the 600-line
//! chunking orchestrator that existed purely to dodge WASM OOM). Native code
//! lays out the full graph in one pass:
//!
//! 1. split into weakly-connected components
//! 2. per component: cycle-break (DFS), longest-path ranking, **virtual-node
//!    expansion of multi-rank edges**, barycenter ordering + transpose
//!    refinement over real *and* virtual nodes, over-tall-rank column
//!    splitting, median y-relaxation
//! 3. edges become smooth polylines routed **through their virtual-node
//!    waypoints** (a Catmull-Rom spline), so long edges follow the lanes the
//!    ordering carved out instead of slicing across the whole picture
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

#[derive(Debug, Clone)]
pub struct EdgePath {
    /// Index into the input edge list.
    pub edge_index: u32,
    /// Flattened smooth polyline in world coords, source port → target.
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
            ordering_sweeps: 8,
            transpose_passes: 3,
        }
    }
}

const VIRTUAL_W: f32 = 8.0;
const VIRTUAL_H: f32 = 8.0;

/// `roots` bias ranking so root types (Query/Mutation) start at rank 0.
pub fn layout(
    nodes: &[LayoutNode],
    edges: &[LayoutEdge],
    roots: &[u32],
    config: &LayoutConfig,
) -> LayoutResult {
    let n = nodes.len();
    let mut positions = vec![Point { x: 0.0, y: 0.0 }; n];
    if n == 0 {
        return LayoutResult { positions, edges: Vec::new(), width: 0.0, height: 0.0 };
    }

    // ---- components (union-find) ----
    let mut uf: Vec<u32> = (0..n as u32).collect();
    fn find(uf: &mut Vec<u32>, x: u32) -> u32 {
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
        let (w, h) = layout_component(
            &members[ci],
            &comp_edges[ci],
            nodes,
            edges,
            &is_root,
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
        let mut ctrl: Vec<Point> = Vec::with_capacity(waypoints.len() + 2);
        ctrl.push(start);
        ctrl.extend_from_slice(waypoints);
        ctrl.push(end);
        edge_paths.push(EdgePath {
            edge_index: ei as u32,
            points: smooth_polyline(&ctrl),
        });
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

/// Centripetal-ish Catmull-Rom through the control points, flattened.
fn smooth_polyline(ctrl: &[Point]) -> Vec<Point> {
    if ctrl.len() <= 2 {
        // straight segment still gets samples for the LOD stride
        let (a, b) = (ctrl[0], *ctrl.last().unwrap());
        return (0..=16)
            .map(|i| {
                let t = i as f32 / 16.0;
                Point { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t }
            })
            .collect();
    }
    let steps_per_seg = 12usize;
    let mut out = Vec::with_capacity((ctrl.len() - 1) * steps_per_seg + 1);
    out.push(ctrl[0]);
    for i in 0..ctrl.len() - 1 {
        let p0 = if i == 0 { ctrl[0] } else { ctrl[i - 1] };
        let p1 = ctrl[i];
        let p2 = ctrl[i + 1];
        let p3 = if i + 2 < ctrl.len() { ctrl[i + 2] } else { ctrl[i + 1] };
        for s in 1..=steps_per_seg {
            let t = s as f32 / steps_per_seg as f32;
            let t2 = t * t;
            let t3 = t2 * t;
            let x = 0.5
                * ((2.0 * p1.x)
                    + (-p0.x + p2.x) * t
                    + (2.0 * p0.x - 5.0 * p1.x + 4.0 * p2.x - p3.x) * t2
                    + (-p0.x + 3.0 * p1.x - 3.0 * p2.x + p3.x) * t3);
            let y = 0.5
                * ((2.0 * p1.y)
                    + (-p0.y + p2.y) * t
                    + (2.0 * p0.y - 5.0 * p1.y + 4.0 * p2.y - p3.y) * t2
                    + (-p0.y + 3.0 * p1.y - 3.0 * p2.y + p3.y) * t3);
            out.push(Point { x, y });
        }
    }
    out
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

    // ---- longest-path ranking over the DAG ----
    let mut rank = vec![0i32; m];
    {
        let mut out: Vec<Vec<u32>> = vec![Vec::new(); m];
        let mut indeg = vec![0u32; m];
        for &((a, b), _) in &acyclic {
            if a != b {
                out[a as usize].push(b);
                indeg[b as usize] += 1;
            }
        }
        let mut queue: std::collections::VecDeque<u32> = (0..m as u32)
            .filter(|&v| indeg[v as usize] == 0)
            .collect();
        while let Some(v) = queue.pop_front() {
            for &w in &out[v as usize] {
                rank[w as usize] = rank[w as usize].max(rank[v as usize] + 1);
                indeg[w as usize] -= 1;
                if indeg[w as usize] == 0 {
                    queue.push_back(w);
                }
            }
        }
        for (i, &v) in comp.iter().enumerate() {
            if is_root[v as usize] {
                rank[i] = 0;
            }
        }
    }

    // ---- virtual-node expansion ----
    // Every DAG pair spanning >1 rank gets a chain of virtual nodes, one per
    // intermediate rank. All nodes (real + virtual) participate in ordering.
    struct XNode {
        /// Some(local real index) or None for virtual.
        real: Option<u32>,
        rank: i32,
        w: f32,
        h: f32,
    }
    let mut xnodes: Vec<XNode> = (0..m)
        .map(|i| XNode {
            real: Some(i as u32),
            rank: rank[i],
            w: nodes[comp[i] as usize].w,
            h: nodes[comp[i] as usize].h,
        })
        .collect();
    // ordering adjacency (adjacent ranks only) over xnodes
    let mut xout: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut xin: Vec<Vec<u32>> = vec![Vec::new(); m];
    // chains: original directed pair -> virtual xnode ids from source to target
    let mut chains: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
    let mut push_edge = |xout: &mut Vec<Vec<u32>>, xin: &mut Vec<Vec<u32>>, a: u32, b: u32| {
        xout[a as usize].push(b);
        xin[b as usize].push(a);
    };
    for &((a, b), reversed) in &acyclic {
        let (ra, rb) = (rank[a as usize], rank[b as usize]);
        let orig_pair = if reversed { (b, a) } else { (a, b) };
        let span = rb - ra;
        // Only lane-route reasonable spans of edges that asked for it; hub
        // and extreme edges stay direct so virtuals don't flood the ranks.
        if span <= 1 || span > 8 || !routable.contains(&orig_pair) {
            if span >= 1 {
                push_edge(&mut xout, &mut xin, a, b);
            }
            continue;
        }
        let mut prev = a;
        let mut chain = Vec::with_capacity((rb - ra - 1) as usize);
        for r in ra + 1..rb {
            let id = xnodes.len() as u32;
            xnodes.push(XNode { real: None, rank: r, w: VIRTUAL_W, h: VIRTUAL_H });
            xout.push(Vec::new());
            xin.push(Vec::new());
            push_edge(&mut xout, &mut xin, prev, id);
            chain.push(id);
            prev = id;
        }
        push_edge(&mut xout, &mut xin, prev, b);
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

    let median = |vals: &mut Vec<f32>| -> Option<f32> {
        if vals.is_empty() {
            return None;
        }
        vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Some(vals[vals.len() / 2])
    };

    // ---- barycenter/median ordering sweeps ----
    let nranks = ranks.len();
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
    let crossings_between = |a: u32, b: u32, pos: &Vec<u32>, xin: &Vec<Vec<u32>>, xout: &Vec<Vec<u32>>| -> (u32, u32) {
        // crossings contributed by (a before b) vs (b before a), counting
        // both neighbor sides
        let mut ab = 0u32;
        let mut ba = 0u32;
        for nbrs in [&xin, &xout] {
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
    };
    for _ in 0..config.transpose_passes {
        let mut improved = false;
        for ri in 0..nranks {
            let len = ranks[ri].len();
            for i in 0..len.saturating_sub(1) {
                let a = ranks[ri][i];
                let b = ranks[ri][i + 1];
                let (ab, ba) = crossings_between(a, b, &pos_in_rank, &xin, &xout);
                if ba < ab {
                    ranks[ri].swap(i, i + 1);
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

    // ---- split over-tall ranks into consecutive column-ranks ----
    {
        let total_area: f32 = comp
            .iter()
            .map(|&v| nodes[v as usize].w * nodes[v as usize].h)
            .sum();
        let h_cap = (total_area.sqrt() * 1.5).max(4000.0);
        let mut new_ranks: Vec<Vec<u32>> = Vec::with_capacity(ranks.len());
        for bucket in ranks.drain(..) {
            let mut chunk: Vec<u32> = Vec::new();
            let mut chunk_h = 0.0f32;
            for v in bucket {
                // only real nodes count toward the split cap — virtual
                // waypoints are cheap and must not fragment the ranks
                let h = if xnodes[v as usize].real.is_some() {
                    xnodes[v as usize].h + config.node_sep
                } else {
                    0.0
                };
                if !chunk.is_empty() && h > 0.0 && chunk_h + h > h_cap {
                    new_ranks.push(std::mem::take(&mut chunk));
                    chunk_h = 0.0;
                }
                chunk_h += h;
                chunk.push(v);
            }
            if !chunk.is_empty() {
                new_ranks.push(chunk);
            }
        }
        ranks = new_ranks;
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
    let sep_of = |x: &XNode| if x.real.is_some() { config.node_sep } else { 4.0 };
    let mut y = vec![0.0f32; xn];
    for ri in 0..nranks {
        let mut cursor = 0.0f32;
        for &v in &ranks[ri] {
            y[v as usize] = cursor;
            cursor += xnodes[v as usize].h + sep_of(&xnodes[v as usize]);
        }
    }
    // Median pull toward neighbors, then per-rank overlap resolution as an
    // isotonic (PAVA) fit: minimum total movement under the ordering
    // constraint, instead of a forward push that made every rank drift
    // diagonally downward.
    for iter in 0..6 {
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
        let r = layout(&nodes, &edges, &[0], &LayoutConfig::default());
        assert!(r.positions[0].x < r.positions[1].x);
        assert!(r.positions[1].x < r.positions[2].x);
        assert_eq!(r.edges.len(), 2);
        assert!(r.edges.iter().all(|e| e.points.len() >= 2));
    }

    #[test]
    fn cycle_does_not_hang_or_overlap_ranks() {
        let nodes = vec![node(100.0, 40.0); 3];
        let edges = vec![edge(0, 1), edge(1, 2), edge(2, 0)];
        let r = layout(&nodes, &edges, &[], &LayoutConfig::default());
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
        let r = layout(&nodes, &edges, &[0], &LayoutConfig::default());
        let mut ys: Vec<f32> = (1..=10).map(|i| r.positions[i].y).collect();
        ys.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in ys.windows(2) {
            assert!(w[1] - w[0] >= 50.0, "nodes overlap: {} vs {}", w[0], w[1]);
        }
    }

    #[test]
    fn long_edge_routes_through_intermediate_ranks() {
        // 0→1→2→3 chain plus a long edge 0→3: the long edge must get
        // waypoints (virtual nodes) rather than a straight two-point line.
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 1), edge(1, 2), edge(2, 3), edge(0, 3)];
        let r = layout(&nodes, &edges, &[0], &LayoutConfig::default());
        let long = &r.edges[3];
        assert!(long.points.len() > 9, "expected smoothed waypoints, got {}", long.points.len());
    }

    #[test]
    fn separate_components_do_not_overlap() {
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 1), edge(2, 3)];
        let r = layout(&nodes, &edges, &[], &LayoutConfig::default());
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
        let r = layout(&nodes, &edges, &[], &LayoutConfig::default());
        for p in &r.positions {
            assert!(p.x.is_finite() && p.y.is_finite());
        }
        assert!(r.width > 0.0 && r.height > 0.0);
    }
}
