//! GraphQL SDL → graph. A port of the TypeScript core in `src/lib/`.
//!
//! - [`sdl_to_graph`] — the parsing pipeline (`sdl-to-graph.ts`)
//! - [`parse_until`] / [`is_until_expired`] — deprecation sunset dates (`until.ts`)
//! - [`reachable_from`] / [`all_reachable_ids`] — root-relative reachability (`reachable.ts`)
//! - [`weakly_connected_components`] — union-find islands (`components.ts`)

mod components;
mod reachable;
mod sdl;
mod types;
mod until;

pub use components::{weakly_connected_components, Component};
pub use reachable::{
    all_reachable_ids, default_root_ops, reachable_from, ReachableSubgraph, ROOT_OPS,
};
pub use sdl::sdl_to_graph;
pub use types::{
    EdgeKind, EnumValue, GraphArg, GraphEdgeData, GraphField, GraphNodeData, NodeKind, ParsedGraph,
    RootTypeMap, SchemaRemoval, SdlToGraphOptions,
};
pub use until::{is_until_expired, parse_until};

#[cfg(test)]
mod tests;
