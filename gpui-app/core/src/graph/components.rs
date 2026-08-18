//! Weakly-connected components. Port of `src/lib/components.ts`.
//!
//! Nodes that can reach each other when edges are treated as undirected. The
//! layout orchestrator uses this to split a schema into independent subgraphs
//! and lay them out in parallel; because no edges cross component boundaries,
//! per-component layouts merge trivially by translating coordinates.

use std::collections::{HashMap, HashSet};

use super::types::{GraphEdgeData, GraphNodeData};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub node_ids: HashSet<String>,
}

/// Union-find over node indices. Components come back in the order their
/// first member appears in `nodes`.
pub fn weakly_connected_components(
    nodes: &[GraphNodeData],
    edges: &[GraphEdgeData],
) -> Vec<Component> {
    let index: HashMap<&str, usize> =
        nodes.iter().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();

    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    let mut rank: Vec<u32> = vec![0; nodes.len()];

    fn find(parent: &mut [usize], x: usize) -> usize {
        // Iterative path compression.
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        let mut cur = x;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }

    for e in edges {
        let (Some(&a), Some(&b)) = (index.get(e.source.as_str()), index.get(e.target.as_str()))
        else {
            continue;
        };
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra == rb {
            continue;
        }
        match rank[ra].cmp(&rank[rb]) {
            std::cmp::Ordering::Less => parent[ra] = rb,
            std::cmp::Ordering::Greater => parent[rb] = ra,
            std::cmp::Ordering::Equal => {
                parent[rb] = ra;
                rank[ra] += 1;
            }
        }
    }

    let mut bucket_of: HashMap<usize, usize> = HashMap::new();
    let mut buckets: Vec<HashSet<String>> = Vec::new();
    for (i, n) in nodes.iter().enumerate() {
        let root = find(&mut parent, i);
        let slot = *bucket_of.entry(root).or_insert_with(|| {
            buckets.push(HashSet::new());
            buckets.len() - 1
        });
        buckets[slot].insert(n.id.clone());
    }

    buckets.into_iter().map(|node_ids| Component { node_ids }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::sdl_to_graph;

    fn sizes(components: &[Component]) -> Vec<usize> {
        components.iter().map(|c| c.node_ids.len()).collect()
    }

    #[test]
    fn a_connected_schema_is_one_component() {
        let g = sdl_to_graph(
            "type Query { me: User } type User { id: ID! posts: [Post!]! } type Post { id: ID! }",
            &Default::default(),
        );
        let components = weakly_connected_components(&g.nodes, &g.edges);
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].node_ids.len(), 3);
    }

    #[test]
    fn disjoint_islands_split() {
        let g = sdl_to_graph(
            r#"
            type Query { me: User }
            type User { id: ID! }
            type Island { peer: Peer }
            type Peer { id: ID! }
            type Lonely { id: ID! }
            "#,
            &Default::default(),
        );
        let components = weakly_connected_components(&g.nodes, &g.edges);
        // Query+User, Island+Peer, Lonely — in first-appearance order.
        assert_eq!(sizes(&components), vec![2, 2, 1]);
        assert!(components[0].node_ids.contains("Query"));
        assert!(components[2].node_ids.contains("Lonely"));
    }

    #[test]
    fn edges_are_undirected_for_this_purpose() {
        // The implements edge runs Interface → Post, yet a walk in either
        // direction must land both in the same component.
        let g = sdl_to_graph(
            "interface Node2 { id: ID! } type Post implements Node2 { id: ID! }",
            &Default::default(),
        );
        let components = weakly_connected_components(&g.nodes, &g.edges);
        assert_eq!(components.len(), 1);
    }

    #[test]
    fn edges_touching_unknown_nodes_are_ignored() {
        let nodes = vec![
            GraphNodeData::new("A", crate::graph::NodeKind::Object),
            GraphNodeData::new("B", crate::graph::NodeKind::Object),
        ];
        let edges = vec![GraphEdgeData::new(
            "A.x->Ghost",
            "A",
            "Ghost",
            crate::graph::EdgeKind::Field,
        )];
        let components = weakly_connected_components(&nodes, &edges);
        assert_eq!(components.len(), 2);
    }

    #[test]
    fn an_empty_graph_has_no_components() {
        assert!(weakly_connected_components(&[], &[]).is_empty());
    }
}
