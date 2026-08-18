//! Graph data model — a faithful port of the TypeScript interfaces in
//! `src/lib/sdl-to-graph.ts`.
//!
//! Optionality follows the TS shape: a `?` field becomes `Option<…>` when
//! the difference between "absent" and "empty" is observable (e.g. `fields`
//! vs `values` distinguishing a node's kind), and a plain `bool` when the
//! TS only ever writes `true` (`isDeprecated`, `isOverlay`, …).

/// The six type-system kinds a schema node can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKind {
    Object,
    Interface,
    Union,
    Enum,
    Scalar,
    Input,
}

impl NodeKind {
    /// The TS discriminator string (`"Object"`, `"Interface"`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Object => "Object",
            NodeKind::Interface => "Interface",
            NodeKind::Union => "Union",
            NodeKind::Enum => "Enum",
            NodeKind::Scalar => "Scalar",
            NodeKind::Input => "Input",
        }
    }

    /// SDL keyword used when reporting an extension against this kind.
    pub fn sdl_keyword(self) -> &'static str {
        match self {
            NodeKind::Object => "type",
            NodeKind::Interface => "interface",
            NodeKind::Union => "union",
            NodeKind::Enum => "enum",
            NodeKind::Scalar => "scalar",
            NodeKind::Input => "input",
        }
    }
}

impl std::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry of a field's argument list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphArg {
    pub name: String,
    /// Rendered type, e.g. `[String!]!`.
    pub type_: String,
    /// Underlying named type, e.g. `String`.
    pub type_name: String,
}

/// A field row on an Object / Interface / Input node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphField {
    pub name: String,
    /// Rendered type as written in the SDL, e.g. `[User!]!`.
    pub type_: String,
    /// Underlying named type. Rewritten to the node type when a Relay
    /// Connection/Edge wrapper is unwrapped.
    pub type_name: String,
    pub nullable: bool,
    /// Set when `type_name` was rewritten past a Relay Connection/Edge.
    pub is_relay_connection: bool,
    /// `None` for input-object fields, which have no argument list at all.
    pub args: Option<Vec<GraphArg>>,
    pub description: Option<String>,
    pub is_deprecated: bool,
    pub deprecation_reason: Option<String>,
    /// Sunset date (`YYYY-MM-DD`) parsed from a `[until …]` marker in the
    /// deprecation reason, when present.
    pub until: Option<String>,
    /// Set when this field came from the temporary overlay SDL rather than
    /// the loaded schema.
    pub is_overlay: bool,
}

/// One value of an Enum node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnumValue {
    pub name: String,
    pub description: Option<String>,
    pub is_deprecated: bool,
    pub deprecation_reason: Option<String>,
    /// See [`GraphField::until`].
    pub until: Option<String>,
    /// See [`GraphField::is_overlay`].
    pub is_overlay: bool,
}

/// A schema type rendered as a graph node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNodeData {
    /// Equals `name` — type names are the graph's identity.
    pub id: String,
    pub name: String,
    pub kind: NodeKind,
    pub description: Option<String>,
    /// `Some` for Object / Interface / Input, `None` otherwise.
    pub fields: Option<Vec<GraphField>>,
    /// `Some` for Enum only.
    pub values: Option<Vec<EnumValue>>,
    /// `Some` for Union only.
    pub members: Option<Vec<String>>,
    /// `Some` for Object / Interface (possibly empty), `None` otherwise.
    pub interfaces: Option<Vec<String>>,
    /// For Object types: names of Unions that include this type, sorted.
    /// Only ever `Some` when non-empty.
    pub member_of_unions: Option<Vec<String>>,
    /// Set when the whole type is new in the overlay.
    pub is_overlay: bool,
}

impl GraphNodeData {
    /// A bare node with every optional slot empty.
    pub fn new(name: impl Into<String>, kind: NodeKind) -> Self {
        let name = name.into();
        GraphNodeData {
            id: name.clone(),
            name,
            kind,
            description: None,
            fields: None,
            values: None,
            members: None,
            interfaces: None,
            member_of_unions: None,
            is_overlay: false,
        }
    }
}

/// What an edge represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    Field,
    Implements,
    Union,
    Arg,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Field => "field",
            EdgeKind::Implements => "implements",
            EdgeKind::Union => "union",
            EdgeKind::Arg => "arg",
        }
    }
}

impl std::fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A directed edge between two graph nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdgeData {
    pub id: String,
    pub source: String,
    pub target: String,
    pub source_field: Option<String>,
    pub source_field_index: Option<usize>,
    pub label: Option<String>,
    pub kind: EdgeKind,
    pub nullable: Option<bool>,
    /// Set on a bundled edge that collapses several field edges sharing the
    /// same source→target into one arrow. Never produced by
    /// [`crate::graph::sdl_to_graph`]; the renderer fills it in.
    pub bundled_labels: Option<Vec<String>>,
}

impl GraphEdgeData {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        target: impl Into<String>,
        kind: EdgeKind,
    ) -> Self {
        GraphEdgeData {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            source_field: None,
            source_field_index: None,
            label: None,
            kind,
            nullable: None,
            bundled_labels: None,
        }
    }
}

/// Resolved names of the three root operation types. A slot is `None` when
/// the schema declares no such root.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RootTypeMap {
    pub query: Option<String>,
    pub mutation: Option<String>,
    pub subscription: Option<String>,
}

/// The result of parsing an SDL document into a graph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedGraph {
    pub nodes: Vec<GraphNodeData>,
    pub edges: Vec<GraphEdgeData>,
    /// `Some` only when the document failed to parse; nodes/edges are then
    /// empty, matching the TS.
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub root_types: RootTypeMap,
}

/// A member — or a whole type — to delete from the graph once every
/// `extend` block has been folded in. SDL has no syntax for taking
/// something away, so subtraction is passed alongside the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaRemoval {
    /// Name of the type the removal targets.
    pub type_: String,
    /// Field, enum value, union member or implemented interface to drop.
    /// `None` drops the whole type.
    pub member: Option<String>,
}

impl SchemaRemoval {
    /// Drop a whole type.
    pub fn type_only(type_: impl Into<String>) -> Self {
        SchemaRemoval { type_: type_.into(), member: None }
    }

    /// Drop one member of a type.
    pub fn member(type_: impl Into<String>, member: impl Into<String>) -> Self {
        SchemaRemoval { type_: type_.into(), member: Some(member.into()) }
    }
}

/// Knobs on [`crate::graph::sdl_to_graph`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdlToGraphOptions {
    /// When true (default), the standard Relay `Node` interface, `PageInfo`,
    /// and `*Edge` / `*Connection` types are folded away and field types are
    /// unwrapped to the underlying node.
    pub hide_relay_boilerplate: bool,
    /// Members to delete after extensions are applied. Runs before Relay
    /// unwrapping and edge building, so a removed field takes its edges with it.
    pub remove: Vec<SchemaRemoval>,
    /// When true, a field or enum value an extension re-declares replaces the
    /// existing one in place instead of raising a duplicate warning.
    pub override_duplicates: bool,
}

impl Default for SdlToGraphOptions {
    fn default() -> Self {
        SdlToGraphOptions {
            hide_relay_boilerplate: true,
            remove: Vec::new(),
            override_duplicates: false,
        }
    }
}
