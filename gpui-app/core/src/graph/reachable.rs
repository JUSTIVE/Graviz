//! Reachability from a root operation type. Port of `src/lib/reachable.ts`.

use std::collections::{HashMap, HashSet, VecDeque};

use super::types::{EdgeKind, GraphEdgeData, GraphNodeData};

/// Default root operation type names, used when a schema declares no explicit
/// `schema { query: … }` overrides.
pub const ROOT_OPS: [&str; 3] = ["Query", "Mutation", "Subscription"];

/// [`ROOT_OPS`] as an owned set, for callers that have nothing schema-specific.
pub fn default_root_ops() -> HashSet<String> {
    ROOT_OPS.iter().map(|s| s.to_string()).collect()
}

/// The slice of a graph reachable from one root, borrowed from the input.
#[derive(Debug)]
pub struct ReachableSubgraph<'a> {
    pub nodes: Vec<&'a GraphNodeData>,
    pub edges: Vec<&'a GraphEdgeData>,
}

/// Node ids reachable from `root_id`, excluding the *other* root operations
/// (so a Query-rooted view doesn't spill into Mutation's half of the schema).
///
/// Returns an empty set when `root_id` names no node.
fn visit(
    nodes: &[GraphNodeData],
    edges: &[GraphEdgeData],
    root_id: &str,
    root_ops: &HashSet<String>,
) -> Option<HashSet<String>> {
    if !nodes.iter().any(|n| n.id == root_id) {
        return None;
    }

    let excluded: HashSet<&str> = root_ops
        .iter()
        .filter(|r| r.as_str() != root_id && nodes.iter().any(|n| n.id == **r))
        .map(|r| r.as_str())
        .collect();

    // Forward adjacency (field / union / arg / implements edges). Implements
    // edges flow Interface → ConcreteType, so `adj` already surfaces every
    // implementor when its interface is visited.
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    // Reverse adjacency for implements: reaching a concrete type also surfaces
    // the interface(s) it implements. Without this, a BFS rooted at (or
    // arriving via) a concrete type would never traverse "up" to the
    // interface — there's no forward edge in that direction.
    let mut impl_rev: HashMap<&str, Vec<&str>> = HashMap::new();

    for e in edges {
        if excluded.contains(e.target.as_str()) {
            continue;
        }
        adj.entry(e.source.as_str()).or_default().push(e.target.as_str());
        if e.kind == EdgeKind::Implements {
            impl_rev.entry(e.target.as_str()).or_default().push(e.source.as_str());
        }
    }

    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(root_id.to_string());
    let mut queue: VecDeque<&str> = VecDeque::new();
    // Borrow the root id from `nodes` so the queue can hold `&str`.
    let root_ref = nodes.iter().find(|n| n.id == root_id).map(|n| n.id.as_str()).unwrap();
    queue.push_back(root_ref);

    while let Some(id) = queue.pop_front() {
        let next = adj.get(id).into_iter().chain(impl_rev.get(id)).flatten();
        for nb in next {
            if excluded.contains(nb) || visited.contains(*nb) {
                continue;
            }
            visited.insert(nb.to_string());
            queue.push_back(nb);
        }
    }

    Some(visited)
}

/// Nodes and edges reachable from `root_id`.
pub fn reachable_from<'a>(
    nodes: &'a [GraphNodeData],
    edges: &'a [GraphEdgeData],
    root_id: &str,
    root_ops: &HashSet<String>,
) -> ReachableSubgraph<'a> {
    let Some(visited) = visit(nodes, edges, root_id, root_ops) else {
        return ReachableSubgraph { nodes: Vec::new(), edges: Vec::new() };
    };
    ReachableSubgraph {
        nodes: nodes.iter().filter(|n| visited.contains(&n.id)).collect(),
        edges: edges
            .iter()
            .filter(|e| visited.contains(&e.source) && visited.contains(&e.target))
            .collect(),
    }
}

/// The set of node ids reachable from *any* root operation in the schema.
/// Used to determine which types are truly orphaned.
pub fn all_reachable_ids(
    nodes: &[GraphNodeData],
    edges: &[GraphEdgeData],
    root_ops: &HashSet<String>,
) -> HashSet<String> {
    let mut ids = HashSet::new();
    for root in root_ops {
        if let Some(reached) = visit(nodes, edges, root, root_ops) {
            ids.extend(reached);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{sdl_to_graph, SdlToGraphOptions};

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reachability_traverses_from_a_renamed_query_root() {
        let g = sdl_to_graph(
            r#"
            schema { query: QueryRoot }
            type QueryRoot { me: User }
            type User { id: ID! friend: Friend }
            type Friend { id: ID! }
            type Orphan { id: ID! }
            "#,
            &SdlToGraphOptions::default(),
        );
        let root_ops = set(&["QueryRoot"]);

        let sub = reachable_from(&g.nodes, &g.edges, "QueryRoot", &root_ops);
        let ids: HashSet<&str> = sub.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains("QueryRoot"));
        assert!(ids.contains("User"));
        assert!(ids.contains("Friend"));
        assert!(!ids.contains("Orphan"));

        // Orphan detection keys off the renamed root, not the literal "Query".
        let reachable = all_reachable_ids(&g.nodes, &g.edges, &root_ops);
        assert!(!reachable.contains("Orphan"));
        assert!(reachable.contains("User"));
    }

    #[test]
    fn unknown_root_yields_an_empty_subgraph() {
        let g = sdl_to_graph("type Query { me: User } type User { id: ID! }", &Default::default());
        let sub = reachable_from(&g.nodes, &g.edges, "Nope", &default_root_ops());
        assert!(sub.nodes.is_empty());
        assert!(sub.edges.is_empty());
    }

    #[test]
    fn other_root_operations_are_excluded_from_the_walk() {
        let g = sdl_to_graph(
            r#"
            type Query { me: User }
            type Mutation { touch: User }
            type User { id: ID! }
            "#,
            &Default::default(),
        );
        let sub = reachable_from(&g.nodes, &g.edges, "Query", &default_root_ops());
        let ids: HashSet<&str> = sub.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["Query", "User"].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn implements_edges_are_walked_in_both_directions() {
        // Query → Post (forward field edge), Post → Timestamped (reverse
        // implements), Timestamped → Comment (forward implements).
        let g = sdl_to_graph(
            r#"
            type Query { post: Post }
            interface Timestamped { createdAt: String }
            type Post implements Timestamped { createdAt: String }
            type Comment implements Timestamped { createdAt: String }
            "#,
            &Default::default(),
        );
        let sub = reachable_from(&g.nodes, &g.edges, "Query", &default_root_ops());
        let ids: HashSet<&str> = sub.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains("Timestamped"));
        assert!(ids.contains("Comment"));
    }

    #[test]
    fn all_reachable_ids_unions_every_root() {
        let g = sdl_to_graph(
            r#"
            type Query { me: User }
            type Mutation { save: Draft }
            type User { id: ID! }
            type Draft { id: ID! }
            type Orphan { id: ID! }
            "#,
            &Default::default(),
        );
        let ids = all_reachable_ids(&g.nodes, &g.edges, &default_root_ops());
        assert!(ids.contains("User"));
        assert!(ids.contains("Draft"));
        assert!(!ids.contains("Orphan"));
    }
}
