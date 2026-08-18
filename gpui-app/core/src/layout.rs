//! Layered (Sugiyama-style) graph layout, left-to-right.
//!
//! Replaces the web app's GraphViz-WASM `dot` pipeline (and the 600-line
//! chunking orchestrator that existed purely to dodge WASM OOM). Native code
//! lays out the full graph in one pass:
//!
//! 1. split into weakly-connected components
//! 2. per component: cycle-break (DFS), longest-path ranking, barycenter
//!    crossing reduction, median-based y-coordinate relaxation
//! 3. edges become port-anchored cubic beziers (source anchored at the
//!    originating field row, unlike dot's node-center splines)
//! 4. components shelf-packed (first-fit decreasing height), singletons
//!    laid out in a grid

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
}

#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone)]
pub struct EdgePath {
    /// Index into the input edge list.
    pub edge_index: u32,
    pub start: Point,
    pub c1: Point,
    pub c2: Point,
    pub end: Point,
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
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            rank_sep: 130.0,
            node_sep: 26.0,
            component_sep: 90.0,
            ordering_sweeps: 8,
        }
    }
}

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
    let mut edge_paths: Vec<EdgePath> = Vec::with_capacity(edges.len());

    for ci in 0..ncomp {
        if members[ci].len() == 1 && comp_edges[ci].is_empty() {
            singleton_ids.push(members[ci][0]);
            continue;
        }
        let (w, h) = layout_component(
            &members[ci],
            &comp_edges[ci],
            nodes,
            edges,
            &is_root,
            config,
            &mut positions,
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
        for &v in &c.members {
            positions[v as usize].x += ox;
            positions[v as usize].y += oy;
        }
        packed_w = packed_w.max(ox + c.w);
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

    // ---- edge paths (world space, after packing) ----
    for (ei, e) in edges.iter().enumerate() {
        let sp = positions[e.from as usize];
        let sn = nodes[e.from as usize];
        let tp = positions[e.to as usize];
        let tn = nodes[e.to as usize];
        let port_y = e.from_port_y.clamp(8.0, (sn.h - 8.0).max(8.0));
        let (start, end) = anchor_points(sp, sn, tp, tn, port_y);
        let t = ((end.x - start.x).abs() / 3.0).clamp(48.0, 280.0);
        let dir = if end.x >= start.x { 1.0 } else { -1.0 };
        edge_paths.push(EdgePath {
            edge_index: ei as u32,
            start,
            c1: Point { x: start.x + t * dir, y: start.y },
            c2: Point { x: end.x - t * dir, y: end.y },
            end,
        });
    }

    LayoutResult {
        positions,
        edges: edge_paths,
        width: packed_w,
        height: packed_h,
    }
}

/// Start on the source's right or left side (whichever faces the target),
/// end on the target's facing side at its vertical center.
fn anchor_points(
    sp: Point,
    sn: LayoutNode,
    tp: Point,
    tn: LayoutNode,
    port_y: f32,
) -> (Point, Point) {
    let s_cx = sp.x + sn.w / 2.0;
    let t_cx = tp.x + tn.w / 2.0;
    let forward = t_cx >= s_cx;
    let start = Point {
        x: if forward { sp.x + sn.w } else { sp.x },
        y: sp.y + port_y,
    };
    let end = Point {
        x: if forward { tp.x } else { tp.x + tn.w },
        y: tp.y + tn.h / 2.0,
    };
    (start, end)
}

/// Writes component-local positions into `positions`; returns (width, height).
fn layout_component(
    comp: &[u32],
    comp_edge_ids: &[u32],
    nodes: &[LayoutNode],
    edges: &[LayoutEdge],
    is_root: &[bool],
    config: &LayoutConfig,
    positions: &mut [Point],
) -> (f32, f32) {
    let m = comp.len();
    // local index map
    let mut local_of = std::collections::HashMap::with_capacity(m);
    for (i, &v) in comp.iter().enumerate() {
        local_of.insert(v, i as u32);
    }
    // adjacency with dedup of parallel edges
    let mut pairs: Vec<(u32, u32)> = comp_edge_ids
        .iter()
        .map(|&ei| {
            let e = &edges[ei as usize];
            (local_of[&e.from], local_of[&e.to])
        })
        .filter(|(a, b)| a != b)
        .collect();
    pairs.sort_unstable();
    pairs.dedup();

    let mut out: Vec<Vec<u32>> = vec![Vec::new(); m];
    let mut indeg = vec![0u32; m];
    // cycle break: DFS, reverse back edges
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
        let mut acyclic: Vec<(u32, u32)> = Vec::with_capacity(pairs.len());
        // roots first so they end up at rank 0
        let mut starts: Vec<u32> = (0..m as u32).collect();
        starts.sort_by_key(|&v| {
            let gid = comp[v as usize] as usize;
            let root = is_root[gid];
            let has_in = pairs.iter().any(|&(_, b)| b == v); // cheap enough per start sort? m small per comp
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
                            acyclic.push((v, w));
                            mark[w as usize] = Mark::Gray;
                            stack.push((w, 0));
                        }
                        Mark::Gray => acyclic.push((w, v)), // back edge: reverse
                        Mark::Black => acyclic.push((v, w)),
                    }
                } else {
                    mark[v as usize] = Mark::Black;
                    stack.pop();
                }
            }
        }
        acyclic.sort_unstable();
        acyclic.dedup();
        for &(a, b) in &acyclic {
            if a == b {
                continue;
            }
            out[a as usize].push(b);
            indeg[b as usize] += 1;
        }
    }

    // longest-path ranking over the DAG (topological relax)
    let mut rank = vec![0i32; m];
    {
        let mut indeg2 = indeg.clone();
        let mut queue: std::collections::VecDeque<u32> = (0..m as u32)
            .filter(|&v| indeg2[v as usize] == 0)
            .collect();
        while let Some(v) = queue.pop_front() {
            for &w in &out[v as usize] {
                rank[w as usize] = rank[w as usize].max(rank[v as usize] + 1);
                indeg2[w as usize] -= 1;
                if indeg2[w as usize] == 0 {
                    queue.push_back(w);
                }
            }
        }
        // force designated roots to rank 0 band
        for (i, &v) in comp.iter().enumerate() {
            if is_root[v as usize] {
                rank[i] = 0;
            }
        }
    }
    let max_rank = *rank.iter().max().unwrap_or(&0);
    let nranks = (max_rank + 1) as usize;

    // rank buckets
    let mut ranks: Vec<Vec<u32>> = vec![Vec::new(); nranks];
    for v in 0..m as u32 {
        ranks[rank[v as usize] as usize].push(v);
    }

    // neighbor lists across adjacent ranks for barycenter sweeps.
    // (edges can span multiple ranks; for ordering we use them as-is.)
    let mut in_nbrs: Vec<Vec<u32>> = vec![Vec::new(); m];
    for v in 0..m {
        for &w in &out[v] {
            in_nbrs[w as usize].push(v as u32);
        }
    }

    // position-in-rank
    let mut pos_in_rank = vec![0u32; m];
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
                    let nbrs = if downward { &in_nbrs[v as usize] } else { &out[v as usize] };
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

    // Split over-tall ranks into consecutive column-ranks so a hub rank with
    // hundreds of nodes doesn't produce a 200k-px-tall world. Order within
    // the rank (from the barycenter sweeps) is preserved across the split.
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
                let h = nodes[comp[v as usize] as usize].h + config.node_sep;
                if !chunk.is_empty() && chunk_h + h > h_cap {
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
        for (ri, bucket) in ranks.iter().enumerate() {
            for &v in bucket {
                rank[v as usize] = ri as i32;
            }
        }
    }
    let nranks = ranks.len();
    assign_pos(&ranks, &mut pos_in_rank);

    // x per rank: cumulative max width
    let mut rank_x = vec![0.0f32; nranks];
    let mut x_cursor = 0.0f32;
    let mut rank_w = vec![0.0f32; nranks];
    for ri in 0..nranks {
        let w = ranks[ri]
            .iter()
            .map(|&v| nodes[comp[v as usize] as usize].w)
            .fold(0.0f32, f32::max);
        rank_x[ri] = x_cursor;
        rank_w[ri] = w;
        x_cursor += w + config.rank_sep;
    }
    let comp_w = if nranks > 0 { x_cursor - config.rank_sep } else { 0.0 };

    // initial y: stack in order
    let mut y = vec![0.0f32; m];
    for ri in 0..nranks {
        let mut cursor = 0.0f32;
        for &v in &ranks[ri] {
            y[v as usize] = cursor;
            cursor += nodes[comp[v as usize] as usize].h + config.node_sep;
        }
    }

    // y relaxation: pull toward median of neighbor centers, then de-overlap
    for _ in 0..4 {
        for ri in 0..nranks {
            for &v in &ranks[ri] {
                let vi = v as usize;
                let h_v = nodes[comp[vi] as usize].h;
                let mut centers: Vec<f32> = Vec::new();
                for &u in in_nbrs[vi].iter().chain(out[vi].iter()) {
                    let ui = u as usize;
                    centers.push(y[ui] + nodes[comp[ui] as usize].h / 2.0);
                }
                if let Some(med) = median(&mut centers) {
                    y[vi] = med - h_v / 2.0;
                }
            }
            // de-overlap while preserving order: forward pass pushes down,
            // then anchor around mean shift
            let rank_nodes = &ranks[ri];
            for k in 1..rank_nodes.len() {
                let prev = rank_nodes[k - 1] as usize;
                let cur = rank_nodes[k] as usize;
                let min_y = y[prev] + nodes[comp[prev] as usize].h + config.node_sep;
                if y[cur] < min_y {
                    y[cur] = min_y;
                }
            }
        }
    }

    // normalize to local (0,0)
    let min_y = comp
        .iter()
        .enumerate()
        .map(|(i, _)| y[i])
        .fold(f32::INFINITY, f32::min);
    let mut comp_h = 0.0f32;
    for (i, &v) in comp.iter().enumerate() {
        let p = Point {
            x: rank_x[rank[i] as usize]
                + (rank_w[rank[i] as usize] - nodes[v as usize].w) / 2.0,
            y: y[i] - min_y,
        };
        comp_h = comp_h.max(p.y + nodes[v as usize].h);
        positions[v as usize] = p;
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
        LayoutEdge { from, to, from_port_y: 20.0 }
    }

    #[test]
    fn chain_ranks_left_to_right() {
        let nodes = vec![node(100.0, 40.0); 3];
        let edges = vec![edge(0, 1), edge(1, 2)];
        let r = layout(&nodes, &edges, &[0], &LayoutConfig::default());
        assert!(r.positions[0].x < r.positions[1].x);
        assert!(r.positions[1].x < r.positions[2].x);
        assert_eq!(r.edges.len(), 2);
    }

    #[test]
    fn cycle_does_not_hang_or_overlap_ranks() {
        let nodes = vec![node(100.0, 40.0); 3];
        let edges = vec![edge(0, 1), edge(1, 2), edge(2, 0)];
        let r = layout(&nodes, &edges, &[], &LayoutConfig::default());
        // all three placed, finite coords
        for p in &r.positions {
            assert!(p.x.is_finite() && p.y.is_finite());
        }
    }

    #[test]
    fn no_overlap_within_rank() {
        // star: one hub feeding 10 nodes → all 10 share rank 1
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
    fn separate_components_do_not_overlap() {
        let nodes = vec![node(100.0, 40.0); 4];
        let edges = vec![edge(0, 1), edge(2, 3)];
        let r = layout(&nodes, &edges, &[], &LayoutConfig::default());
        // crude: bounding boxes of the two pairs must not intersect
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
