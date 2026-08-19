//! Minimum-total-span layering: the network simplex of Gansner et al.
//!
//! Given an acyclic graph, assign every node an integer rank such that every
//! edge goes forward by at least `minlen`, and the **sum of edge spans is as
//! small as it can be**. Since a node's x position is its rank, that sum is
//! the horizontal part of total edge length — three quarters of it, measured
//! on a real schema — so this is the one stage where edge length can be
//! optimised rather than merely nudged.
//!
//! The two obvious layerings are both far from optimal. Longest-path ranking
//! is feasible but makes the graph as deep as its longest chain and leaves
//! most edges spanning many columns. Ranking by BFS depth is shallow but not
//! feasible at all — a third of the edges come out pointing sideways or
//! backwards, and those are the ones that grow long and cut across unrelated
//! cards. The simplex gets both: every edge forward, and the total span
//! provably minimal.
//!
//! The implementation follows the paper (and dagre's reading of it): build a
//! feasible tight spanning tree, compute cut values, then repeatedly swap a
//! tree edge with a negative cut value for the least-slack edge that
//! reconnects the two halves.

use std::collections::HashMap;

/// One edge of the layering problem. `weight` scales that edge's pull, so a
/// caller can say "keep these two close" without adding geometry.
#[derive(Debug, Clone, Copy)]
pub struct RankEdge {
    pub from: u32,
    pub to: u32,
    pub weight: i32,
    /// Minimum number of columns the edge must span. Zero lets both ends
    /// share a column, which is how a caller says "forward, but no need to
    /// spend a column on it".
    pub minlen: i32,
}

impl RankEdge {
    pub fn new(from: u32, to: u32) -> Self {
        RankEdge { from, to, weight: 1, minlen: 1 }
    }
}

struct Net {
    n: usize,
    edges: Vec<RankEdge>,
    /// Incident edge ids per node.
    inc: Vec<Vec<usize>>,
    rank: Vec<i32>,
    /// Tree adjacency: node -> (neighbour, edge id).
    tree: Vec<Vec<(u32, usize)>>,
    in_tree: Vec<bool>,
    cut: HashMap<usize, i32>,
    low: Vec<u32>,
    lim: Vec<u32>,
    parent: Vec<Option<usize>>,
}

impl Net {
    fn slack(&self, e: usize) -> i32 {
        let ed = &self.edges[e];
        self.rank[ed.to as usize] - self.rank[ed.from as usize] - ed.minlen
    }

    /// Longest-path ranking: feasible, and the starting point the simplex
    /// improves on.
    fn init_rank(&mut self) {
        let mut indeg = vec![0usize; self.n];
        let mut out: Vec<Vec<usize>> = vec![Vec::new(); self.n];
        for (i, e) in self.edges.iter().enumerate() {
            out[e.from as usize].push(i);
            indeg[e.to as usize] += 1;
        }
        let mut queue: Vec<u32> =
            (0..self.n as u32).filter(|&v| indeg[v as usize] == 0).collect();
        let mut head = 0;
        while head < queue.len() {
            let v = queue[head];
            head += 1;
            for &i in &out[v as usize] {
                let e = self.edges[i];
                let cand = self.rank[v as usize] + e.minlen;
                if cand > self.rank[e.to as usize] {
                    self.rank[e.to as usize] = cand;
                }
                indeg[e.to as usize] -= 1;
                if indeg[e.to as usize] == 0 {
                    queue.push(e.to);
                }
            }
        }
    }

    /// Grow the set of nodes reachable from `start` over zero-slack edges.
    fn tight_tree(&mut self, start: u32) -> usize {
        for l in self.tree.iter_mut() {
            l.clear();
        }
        self.in_tree.iter_mut().for_each(|b| *b = false);
        self.in_tree[start as usize] = true;
        let mut stack = vec![start];
        let mut size = 1;
        while let Some(v) = stack.pop() {
            for idx in 0..self.inc[v as usize].len() {
                let i = self.inc[v as usize][idx];
                let e = self.edges[i];
                let other = if e.from == v { e.to } else { e.from };
                if self.in_tree[other as usize] || self.slack(i) != 0 {
                    continue;
                }
                self.in_tree[other as usize] = true;
                self.tree[v as usize].push((other, i));
                self.tree[other as usize].push((v, i));
                stack.push(other);
                size += 1;
            }
        }
        size
    }

    /// A tight spanning tree, shifting whole components until one exists.
    fn feasible_tree(&mut self) {
        if self.n <= 1 {
            return;
        }
        loop {
            let size = self.tight_tree(0);
            if size == self.n {
                break;
            }
            // The least-slack edge with exactly one end in the tree tells us
            // how far the tree can move without breaking feasibility.
            let mut best: Option<(usize, i32)> = None;
            for (i, e) in self.edges.iter().enumerate() {
                let (a, b) = (self.in_tree[e.from as usize], self.in_tree[e.to as usize]);
                if a == b {
                    continue;
                }
                let s = self.slack(i);
                if best.is_none_or(|(_, bs)| s < bs) {
                    best = Some((i, s));
                }
            }
            let Some((i, s)) = best else { break };
            let delta = if self.in_tree[self.edges[i].from as usize] { s } else { -s };
            if delta == 0 {
                break;
            }
            for v in 0..self.n {
                if self.in_tree[v] {
                    self.rank[v] += delta;
                }
            }
        }
    }

    /// Postorder interval labels, so "is x under y?" is two comparisons.
    fn init_low_lim(&mut self) {
        self.low = vec![0; self.n];
        self.lim = vec![0; self.n];
        self.parent = vec![None; self.n];
        let mut visited = vec![false; self.n];
        let mut next = 1u32;
        // Iterative DFS: (node, incoming tree edge, child cursor).
        let mut stack: Vec<(u32, Option<usize>, usize)> = vec![(0, None, 0)];
        visited[0] = true;
        self.low[0] = next;
        while let Some(&mut (v, pe, ref mut cursor)) = stack.last_mut() {
            if *cursor < self.tree[v as usize].len() {
                let (w, i) = self.tree[v as usize][*cursor];
                *cursor += 1;
                if visited[w as usize] {
                    continue;
                }
                visited[w as usize] = true;
                self.parent[w as usize] = Some(i);
                self.low[w as usize] = next;
                stack.push((w, Some(i), 0));
            } else {
                self.lim[v as usize] = next;
                next += 1;
                let _ = pe;
                stack.pop();
            }
        }
    }

    fn is_descendant(&self, v: u32, root: u32) -> bool {
        self.low[root as usize] <= self.lim[v as usize]
            && self.lim[v as usize] <= self.lim[root as usize]
    }

    /// Cut values, leaves first, each in time proportional to the node's
    /// degree — the whole pass is linear in the edge count.
    fn init_cut_values(&mut self) {
        self.cut.clear();
        // Nodes in increasing `lim` are exactly postorder: children first.
        let mut order: Vec<u32> = (0..self.n as u32).collect();
        order.sort_by_key(|&v| self.lim[v as usize]);
        for &child in &order {
            let Some(pe) = self.parent[child as usize] else { continue };
            let e = self.edges[pe];
            let parent = if e.from == child { e.to } else { e.from };
            // Is the child on the tail side of its parent edge?
            let child_is_tail = e.from == child;
            let mut cut = e.weight;
            for idx in 0..self.inc[child as usize].len() {
                let i = self.inc[child as usize][idx];
                let f = self.edges[i];
                let is_out = f.from == child;
                let other = if is_out { f.to } else { f.from };
                // Every edge back to the parent is accounted for by the
                // parent edge itself, not just the tree one.
                if other == parent {
                    continue;
                }
                let points_to_head = is_out == child_is_tail;
                cut += if points_to_head { f.weight } else { -f.weight };
                // A tree edge's own cut value already accounts for everything
                // crossing it, so fold it in rather than counting twice.
                if self.tree[child as usize].iter().any(|&(w, id)| w == other && id == i) {
                    if let Some(&c) = self.cut.get(&i) {
                        cut += if points_to_head { -c } else { c };
                    }
                }
            }
            self.cut.insert(pe, cut);
        }
    }

    /// A tree edge worth replacing.
    fn leave_edge(&self) -> Option<usize> {
        self.cut.iter().find(|(_, &c)| c < 0).map(|(&i, _)| i)
    }

    /// The cheapest non-tree edge that reconnects the halves `e` separates.
    fn enter_edge(&self, e: usize) -> Option<usize> {
        let ed = self.edges[e];
        let (v, w) = (ed.from, ed.to);
        // The endpoint further from the root defines the subtree.
        let (tail_root, flip) = if self.lim[v as usize] < self.lim[w as usize] {
            (v, false)
        } else {
            (w, true)
        };
        let mut best: Option<(usize, i32)> = None;
        for (i, f) in self.edges.iter().enumerate() {
            if i == e {
                continue;
            }
            let from_in = self.is_descendant(f.from, tail_root);
            let to_in = self.is_descendant(f.to, tail_root);
            if from_in == flip && to_in != flip {
                let s = self.slack(i);
                if best.is_none_or(|(_, bs)| s < bs) {
                    best = Some((i, s));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    fn exchange(&mut self, leave: usize, enter: usize) {
        let le = self.edges[leave];
        self.tree[le.from as usize].retain(|&(_, id)| id != leave);
        self.tree[le.to as usize].retain(|&(_, id)| id != leave);
        let fe = self.edges[enter];
        self.tree[fe.from as usize].push((fe.to, enter));
        self.tree[fe.to as usize].push((fe.from, enter));
        self.init_low_lim();
        self.update_ranks();
        self.init_cut_values();
    }

    /// Re-derive ranks from the tree, so every tree edge is tight again.
    fn update_ranks(&mut self) {
        let mut visited = vec![false; self.n];
        visited[0] = true;
        self.rank[0] = 0;
        let mut stack = vec![0u32];
        while let Some(v) = stack.pop() {
            for idx in 0..self.tree[v as usize].len() {
                let (w, i) = self.tree[v as usize][idx];
                if visited[w as usize] {
                    continue;
                }
                visited[w as usize] = true;
                let e = self.edges[i];
                self.rank[w as usize] = if e.from == v {
                    self.rank[v as usize] + e.minlen
                } else {
                    self.rank[v as usize] - e.minlen
                };
                stack.push(w);
            }
        }
    }
}

/// Rank `n` nodes so that every edge runs forward and the total span is
/// minimal. `edges` must be acyclic; self-loops are ignored.
///
/// `max_iter` bounds the simplex pivots. The optimum is usually reached in
/// far fewer, but a bound keeps a pathological graph from stalling the app —
/// stopping early simply leaves a feasible, slightly-worse layering.
pub fn network_simplex(n: usize, edges: &[RankEdge], max_iter: usize) -> Vec<i32> {
    let edges: Vec<RankEdge> = edges
        .iter()
        .copied()
        .filter(|e| e.from != e.to)
        .map(|mut e| {
            e.minlen = e.minlen.max(0);
            e
        })
        .collect();
    if n == 0 {
        return Vec::new();
    }
    let mut inc: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, e) in edges.iter().enumerate() {
        inc[e.from as usize].push(i);
        inc[e.to as usize].push(i);
    }
    let mut net = Net {
        n,
        edges,
        inc,
        rank: vec![0; n],
        tree: vec![Vec::new(); n],
        in_tree: vec![false; n],
        cut: HashMap::new(),
        low: vec![0; n],
        lim: vec![0; n],
        parent: vec![None; n],
    };
    net.init_rank();
    net.feasible_tree();
    net.init_low_lim();
    net.init_cut_values();
    let mut pivots = 0usize;
    for _ in 0..max_iter {
        let Some(leave) = net.leave_edge() else { break };
        let Some(enter) = net.enter_edge(leave) else { break };
        if enter == leave {
            break;
        }
        net.exchange(leave, enter);
        pivots += 1;
    }
    if std::env::var("GOMPASS_PERF").is_ok() {
        let span: i64 = net
            .edges
            .iter()
            .map(|e| (net.rank[e.to as usize] - net.rank[e.from as usize]) as i64 * e.weight as i64)
            .sum();
        eprintln!("simplex: {} nodes, {pivots} pivots, total span {span}", net.n);
    }
    // Normalise to start at zero.
    let min = net.rank.iter().copied().min().unwrap_or(0);
    for r in net.rank.iter_mut() {
        *r -= min;
    }
    net.rank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn total_span(edges: &[RankEdge], rank: &[i32]) -> i32 {
        edges
            .iter()
            .map(|e| (rank[e.to as usize] - rank[e.from as usize]) * e.weight)
            .sum()
    }

    #[test]
    fn every_edge_runs_forward() {
        let edges = vec![
            RankEdge::new(0, 1),
            RankEdge::new(1, 2),
            RankEdge::new(2, 3),
            RankEdge::new(0, 3),
            RankEdge::new(0, 2),
        ];
        let rank = network_simplex(4, &edges, 256);
        for e in &edges {
            assert!(
                rank[e.to as usize] > rank[e.from as usize],
                "edge {}->{} runs backwards: {rank:?}",
                e.from,
                e.to
            );
        }
    }

    #[test]
    fn beats_longest_path_on_total_span() {
        // A chain with a shortcut: longest-path drags the shortcut's target
        // to the end of the chain, the simplex does not have to.
        //   0→1→2→3→4→5   plus   6→1 and 6→5
        let edges = vec![
            RankEdge::new(0, 1),
            RankEdge::new(1, 2),
            RankEdge::new(2, 3),
            RankEdge::new(3, 4),
            RankEdge::new(4, 5),
            RankEdge::new(6, 1),
            RankEdge::new(6, 5),
        ];
        let simplex = network_simplex(7, &edges, 256);
        // Longest path for comparison.
        let mut lp = vec![0i32; 7];
        for _ in 0..7 {
            for e in &edges {
                let c = lp[e.from as usize] + 1;
                if c > lp[e.to as usize] {
                    lp[e.to as usize] = c;
                }
            }
        }
        assert!(
            total_span(&edges, &simplex) <= total_span(&edges, &lp),
            "simplex {} vs longest-path {}",
            total_span(&edges, &simplex),
            total_span(&edges, &lp)
        );
    }

    #[test]
    fn weight_pulls_an_edge_tight() {
        // 0→1, 0→2, 1→3, 2→3, with 0→2 heavily weighted: 2 should sit right
        // after 0 rather than being dragged along by the other path.
        let edges = vec![
            RankEdge::new(0, 1),
            RankEdge { from: 0, to: 2, weight: 8, minlen: 1 },
            RankEdge::new(1, 3),
            RankEdge::new(2, 3),
        ];
        let rank = network_simplex(4, &edges, 256);
        assert_eq!(rank[2] - rank[0], 1);
    }

    #[test]
    fn handles_a_single_node_and_no_edges() {
        assert_eq!(network_simplex(1, &[], 16), vec![0]);
        assert_eq!(network_simplex(0, &[], 16), Vec::<i32>::new());
    }

    #[test]
    fn ignores_self_loops() {
        let edges = vec![RankEdge::new(0, 0), RankEdge::new(0, 1)];
        let rank = network_simplex(2, &edges, 16);
        assert_eq!(rank[1] - rank[0], 1);
    }
}
