//! SDL → graph. A faithful port of `src/lib/sdl-to-graph.ts`.
//!
//! The pipeline runs in the same order as the TS, and each stage below is
//! annotated with the reason that order matters:
//!
//! 1. duplicate / conflicting declaration detection → warnings
//! 2. node building from definitions
//! 3. [`apply_extensions`] — fold every `extend …` into its target
//! 4. [`apply_removals`] — delete members the overlay subtracted
//! 5. Relay unwrapping (`*Edge`, then `*Connection`)
//! 6. edge building
//! 7. `member_of_unions` reverse index
//! 8. [`resolve_root_types`]
//!
//! # Known divergence from the TS
//!
//! graphql-js's `parse` is purely syntactic, while `async_graphql_parser`
//! folds two schema-level validations into parsing. Both cases reject a
//! document the TS would have accepted, surfacing as `error` rather than as a
//! graph:
//!
//! - `schema { mutation: M }` with no `query:` slot → *"schema definition is
//!   missing query root"*. Such a schema is invalid per the spec anyway.
//! - the same operation twice in one `schema { … }` block (`query: A query: B`)
//!   → *"multiple query roots"*. The TS would silently keep the last. Spread
//!   across a `schema` and an `extend schema` block it still works, and the
//!   last one written still wins.
//!
//! Everything else round-trips identically; the port is byte-for-byte
//! equivalent on the production schema (nodes, edges and field rows alike).

use std::collections::{HashMap, HashSet};

use async_graphql_parser::types::{
    BaseType, ConstDirective, EnumValueDefinition, FieldDefinition, InputValueDefinition,
    ServiceDocument, Type, TypeKind, TypeSystemDefinition,
};
use async_graphql_parser::{parse_schema, Positioned};
use async_graphql_value::ConstValue;

use super::types::*;
use super::until::parse_until;

const BUILTIN_SCALARS: [&str; 5] = ["String", "Int", "Float", "Boolean", "ID"];

fn is_builtin_scalar(name: &str) -> bool {
    BUILTIN_SCALARS.contains(&name)
}

/// Parses an SDL document into nodes and edges.
///
/// Never panics and never returns `Err`: a document that fails to parse comes
/// back as an otherwise-empty [`ParsedGraph`] with `error` set, exactly like
/// the TS.
pub fn sdl_to_graph(sdl: &str, options: &SdlToGraphOptions) -> ParsedGraph {
    if sdl.trim().is_empty() {
        return ParsedGraph::default();
    }

    let doc: ServiceDocument = match parse_schema(sdl) {
        Ok(doc) => doc,
        Err(e) => {
            return ParsedGraph { error: Some(e.to_string()), ..ParsedGraph::default() };
        }
    };

    let duplicate_warnings = detect_duplicate_declarations(&doc);
    let mut nodes = build_nodes(&doc);

    // Fold `extend …` blocks into their targets before anything reads the
    // node list — Relay unwrapping, edge building and reachability all then
    // see the extended shape.
    let extension_warnings = apply_extensions(&doc, &mut nodes, options.override_duplicates);

    // Subtraction comes after addition, so `-foo` can take away a field an
    // `extend` block in the same overlay just added.
    let removal_warnings = if options.remove.is_empty() {
        Vec::new()
    } else {
        apply_removals(&mut nodes, &options.remove)
    };

    if options.hide_relay_boilerplate {
        unwrap_relay(&mut nodes);
    }

    let raw_edges = build_edges(&nodes);

    // Drop the Relay boilerplate nodes themselves; any edges still pointing at
    // them are filtered out as dangling below.
    if options.hide_relay_boilerplate {
        nodes.retain(|n| !is_relay_boilerplate(n));
    }

    index_union_membership(&mut nodes);

    let kept_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let edges: Vec<GraphEdgeData> = raw_edges
        .into_iter()
        .filter(|e| kept_ids.contains(e.target.as_str()) && kept_ids.contains(e.source.as_str()))
        .collect();

    let node_names: HashSet<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    let root_types = resolve_root_types(&doc, &node_names);

    let mut warnings = duplicate_warnings;
    warnings.extend(extension_warnings);
    warnings.extend(removal_warnings);

    ParsedGraph { nodes, edges, error: None, warnings, root_types }
}

// ── stage 1: duplicate / conflicting declarations ──────────────────────────

/// Detects duplicate or conflicting type-system declarations. Limited to
/// type/interface/input/enum/union/scalar — directives, `schema`, and
/// `extend` blocks are skipped.
fn detect_duplicate_declarations(doc: &ServiceDocument) -> Vec<String> {
    struct Decl {
        keyword: &'static str,
        line: usize,
        column: usize,
    }
    // Insertion-ordered, to match the TS `Map` iteration.
    let mut order: Vec<String> = Vec::new();
    let mut decls: HashMap<String, Vec<Decl>> = HashMap::new();

    for def in &doc.definitions {
        let TypeSystemDefinition::Type(ty) = def else { continue };
        if ty.node.extend {
            continue;
        }
        let name = ty.node.name.node.as_str().to_string();
        let keyword = kind_of(&ty.node.kind).sdl_keyword();
        let entry = decls.entry(name.clone()).or_insert_with(|| {
            order.push(name.clone());
            Vec::new()
        });
        entry.push(Decl { keyword, line: ty.pos.line, column: ty.pos.column });
    }

    let mut warnings = Vec::new();
    for name in &order {
        let list = &decls[name];
        if list.len() < 2 {
            continue;
        }
        let distinct: HashSet<&str> = list.iter().map(|d| d.keyword).collect();
        if distinct.len() == 1 {
            let positions = list
                .iter()
                .map(|d| format!("{}:{}", d.line, d.column))
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(format!(
                "Duplicate {} \"{}\" at {}",
                list[0].keyword, name, positions
            ));
        } else {
            let positions = list
                .iter()
                .map(|d| format!("{} at {}:{}", d.keyword, d.line, d.column))
                .collect::<Vec<_>>()
                .join(", ");
            warnings.push(format!("Conflicting declarations for \"{}\": {}", name, positions));
        }
    }
    warnings
}

fn kind_of(kind: &TypeKind) -> NodeKind {
    match kind {
        TypeKind::Scalar => NodeKind::Scalar,
        TypeKind::Object(_) => NodeKind::Object,
        TypeKind::Interface(_) => NodeKind::Interface,
        TypeKind::Union(_) => NodeKind::Union,
        TypeKind::Enum(_) => NodeKind::Enum,
        TypeKind::InputObject(_) => NodeKind::Input,
    }
}

// ── stage 2: node building ─────────────────────────────────────────────────

fn build_nodes(doc: &ServiceDocument) -> Vec<GraphNodeData> {
    let mut nodes = Vec::new();
    for def in &doc.definitions {
        let TypeSystemDefinition::Type(ty) = def else { continue };
        if ty.node.extend {
            continue;
        }
        let name = ty.node.name.node.as_str();
        let description = ty.node.description.as_ref().map(|d| d.node.clone());

        let mut node = match &ty.node.kind {
            TypeKind::Object(o) => {
                let mut n = GraphNodeData::new(name, NodeKind::Object);
                n.fields = Some(build_fields(&o.fields));
                n.interfaces =
                    Some(o.implements.iter().map(|i| i.node.as_str().to_string()).collect());
                n
            }
            TypeKind::Interface(i) => {
                let mut n = GraphNodeData::new(name, NodeKind::Interface);
                n.fields = Some(build_fields(&i.fields));
                n.interfaces =
                    Some(i.implements.iter().map(|x| x.node.as_str().to_string()).collect());
                n
            }
            TypeKind::InputObject(i) => {
                let mut n = GraphNodeData::new(name, NodeKind::Input);
                n.fields = Some(build_input_fields(&i.fields));
                n
            }
            TypeKind::Enum(e) => {
                let mut n = GraphNodeData::new(name, NodeKind::Enum);
                n.values = Some(build_enum_values(&e.values));
                n
            }
            TypeKind::Union(u) => {
                let mut n = GraphNodeData::new(name, NodeKind::Union);
                n.members = Some(u.members.iter().map(|m| m.node.as_str().to_string()).collect());
                n
            }
            TypeKind::Scalar => GraphNodeData::new(name, NodeKind::Scalar),
        };
        node.description = description;
        nodes.push(node);
    }
    nodes
}

/// Renders a type as written (`[User!]!`) plus its underlying named type.
fn render_type(t: &Type) -> (String, String) {
    (t.to_string(), base_name(&t.base).to_string())
}

fn base_name(b: &BaseType) -> &str {
    match b {
        BaseType::Named(n) => n.as_str(),
        BaseType::List(inner) => base_name(&inner.base),
    }
}

struct Deprecation {
    is_deprecated: bool,
    reason: Option<String>,
    until: Option<String>,
}

fn parse_deprecated(directives: &[Positioned<ConstDirective>]) -> Deprecation {
    let Some(d) = directives.iter().find(|d| d.node.name.node.as_str() == "deprecated") else {
        return Deprecation { is_deprecated: false, reason: None, until: None };
    };
    let reason = d
        .node
        .arguments
        .iter()
        .find(|(name, _)| name.node.as_str() == "reason")
        .and_then(|(_, value)| match &value.node {
            ConstValue::String(s) => Some(s.clone()),
            _ => None,
        });
    let until = parse_until(reason.as_deref());
    Deprecation { is_deprecated: true, reason, until }
}

/// Builds the graph-level field list for an object/interface definition or
/// extension. Shared so `extend type` blocks produce identical fields.
fn build_fields(defs: &[Positioned<FieldDefinition>]) -> Vec<GraphField> {
    defs.iter()
        .map(|f| {
            let (rendered, base) = render_type(&f.node.ty.node);
            let dep = parse_deprecated(&f.node.directives);
            GraphField {
                name: f.node.name.node.as_str().to_string(),
                type_: rendered,
                type_name: base,
                nullable: f.node.ty.node.nullable,
                is_relay_connection: false,
                args: Some(
                    f.node
                        .arguments
                        .iter()
                        .map(|a| {
                            let (rendered, base) = render_type(&a.node.ty.node);
                            GraphArg {
                                name: a.node.name.node.as_str().to_string(),
                                type_: rendered,
                                type_name: base,
                            }
                        })
                        .collect(),
                ),
                description: f.node.description.as_ref().map(|d| d.node.clone()),
                is_deprecated: dep.is_deprecated,
                deprecation_reason: dep.reason,
                until: dep.until,
                is_overlay: false,
            }
        })
        .collect()
}

/// Input-object fields have no argument list at all — the TS leaves `args`
/// `undefined` for them, which the graph reads as "nothing to chain through".
fn build_input_fields(defs: &[Positioned<InputValueDefinition>]) -> Vec<GraphField> {
    defs.iter()
        .map(|f| {
            let (rendered, base) = render_type(&f.node.ty.node);
            let dep = parse_deprecated(&f.node.directives);
            GraphField {
                name: f.node.name.node.as_str().to_string(),
                type_: rendered,
                type_name: base,
                nullable: f.node.ty.node.nullable,
                is_relay_connection: false,
                args: None,
                description: f.node.description.as_ref().map(|d| d.node.clone()),
                is_deprecated: dep.is_deprecated,
                deprecation_reason: dep.reason,
                until: dep.until,
                is_overlay: false,
            }
        })
        .collect()
}

fn build_enum_values(defs: &[Positioned<EnumValueDefinition>]) -> Vec<EnumValue> {
    defs.iter()
        .map(|v| {
            let dep = parse_deprecated(&v.node.directives);
            EnumValue {
                name: v.node.value.node.as_str().to_string(),
                description: v.node.description.as_ref().map(|d| d.node.clone()),
                is_deprecated: dep.is_deprecated,
                deprecation_reason: dep.reason,
                until: dep.until,
                is_overlay: false,
            }
        })
        .collect()
}

// ── stage 3: extensions ────────────────────────────────────────────────────

/// Folds every `extend type|interface|input|enum|union|scalar` block into the
/// node it targets, mutating `nodes` in place.
///
/// Anything that can't be applied — extending a type that doesn't exist, or a
/// kind mismatch — is reported as a warning and skipped rather than throwing,
/// so a half-written overlay still renders the parts that do resolve.
///
/// With `override_duplicates`, re-declaring an existing field or enum value
/// replaces it *in place* — the card doesn't reshuffle just because a type
/// changed.
fn apply_extensions(
    doc: &ServiceDocument,
    nodes: &mut [GraphNodeData],
    override_duplicates: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    // Last declaration wins, matching the TS `byName.set` loop.
    let mut by_name: HashMap<String, usize> = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        by_name.insert(n.name.clone(), i);
    }

    for def in &doc.definitions {
        let TypeSystemDefinition::Type(ty) = def else { continue };
        if !ty.node.extend {
            continue;
        }
        let expected = kind_of(&ty.node.kind);
        let name = ty.node.name.node.as_str();
        let label = expected.sdl_keyword();
        let where_ = format!(" at {}:{}", ty.pos.line, ty.pos.column);

        let Some(&idx) = by_name.get(name) else {
            warnings.push(format!("Cannot extend unknown {} \"{}\"{}", label, name, where_));
            continue;
        };
        if nodes[idx].kind != expected {
            warnings.push(format!(
                "Cannot extend \"{}\" as {}{} — it is declared as {}",
                name,
                label,
                where_,
                nodes[idx].kind.sdl_keyword(),
            ));
            continue;
        }
        let target = &mut nodes[idx];

        match &ty.node.kind {
            TypeKind::Enum(e) => {
                let values = target.values.get_or_insert_with(Vec::new);
                let mut at: HashMap<String, usize> =
                    values.iter().enumerate().map(|(i, v)| (v.name.clone(), i)).collect();
                for v in build_enum_values(&e.values) {
                    match at.get(&v.name).copied() {
                        None => {
                            at.insert(v.name.clone(), values.len());
                            values.push(v);
                        }
                        Some(i) if override_duplicates => values[i] = v,
                        Some(_) => warnings.push(format!(
                            "Duplicate value \"{}.{}\" in extension{}",
                            name, v.name, where_
                        )),
                    }
                }
            }
            TypeKind::Union(u) => {
                let members = target.members.get_or_insert_with(Vec::new);
                let mut existing: HashSet<String> = members.iter().cloned().collect();
                for m in &u.members {
                    let m = m.node.as_str();
                    if existing.insert(m.to_string()) {
                        members.push(m.to_string());
                    }
                }
            }
            // Scalar extensions only ever carry directives — nothing to merge
            // into the graph, but the target existing means it's still valid.
            TypeKind::Scalar => {}
            TypeKind::Object(_) | TypeKind::Interface(_) | TypeKind::InputObject(_) => {
                let (new_fields, new_interfaces) = match &ty.node.kind {
                    TypeKind::Object(o) => (
                        build_fields(&o.fields),
                        o.implements.iter().map(|i| i.node.as_str().to_string()).collect(),
                    ),
                    TypeKind::Interface(i) => (
                        build_fields(&i.fields),
                        i.implements.iter().map(|x| x.node.as_str().to_string()).collect(),
                    ),
                    TypeKind::InputObject(i) => (build_input_fields(&i.fields), Vec::new()),
                    _ => unreachable!(),
                };

                let fields = target.fields.get_or_insert_with(Vec::new);
                let mut at: HashMap<String, usize> =
                    fields.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
                for f in new_fields {
                    match at.get(&f.name).copied() {
                        None => {
                            at.insert(f.name.clone(), fields.len());
                            fields.push(f);
                        }
                        // In place, so an overridden field keeps its row
                        // position. A block that states the same field twice
                        // lands here too, so the last one written stands.
                        Some(i) if override_duplicates => fields[i] = f,
                        Some(_) => warnings.push(format!(
                            "Duplicate field \"{}.{}\" in extension{}",
                            name, f.name, where_
                        )),
                    }
                }

                let new_interfaces: Vec<String> = new_interfaces;
                if !new_interfaces.is_empty() {
                    let ifaces = target.interfaces.get_or_insert_with(Vec::new);
                    let existing: HashSet<String> = ifaces.iter().cloned().collect();
                    let added: Vec<String> =
                        new_interfaces.into_iter().filter(|i| !existing.contains(i)).collect();
                    ifaces.extend(added);
                }
            }
        }
    }
    warnings
}

// ── stage 4: removals ──────────────────────────────────────────────────────

/// Deletes the requested members from `nodes`, mutating it in place.
///
/// A member name is looked up in whichever list the target kind renders —
/// enum values, union members, otherwise fields — falling back to the
/// implemented-interface list, so `-Node` inside an object block drops the
/// `implements Node` rather than reporting a missing field.
fn apply_removals(nodes: &mut Vec<GraphNodeData>, removals: &[SchemaRemoval]) -> Vec<String> {
    let mut warnings = Vec::new();

    for r in removals {
        let Some(idx) = nodes.iter().position(|n| n.name == r.type_) else {
            warnings.push(match &r.member {
                Some(m) => {
                    format!("Cannot remove \"{}.{}\" — no type \"{}\"", r.type_, m, r.type_)
                }
                None => format!("Cannot remove unknown type \"{}\"", r.type_),
            });
            continue;
        };

        let Some(member) = r.member.as_deref() else {
            nodes.remove(idx);
            continue;
        };

        let target = &mut nodes[idx];
        let dropped = match target.kind {
            NodeKind::Enum => {
                let values = target.values.get_or_insert_with(Vec::new);
                let before = values.len();
                values.retain(|v| v.name != member);
                values.len() < before
            }
            NodeKind::Union => {
                let members = target.members.get_or_insert_with(Vec::new);
                let before = members.len();
                members.retain(|m| m != member);
                members.len() < before
            }
            _ => {
                let fields = target.fields.get_or_insert_with(Vec::new);
                let before = fields.len();
                fields.retain(|f| f.name != member);
                if fields.len() < before {
                    true
                } else {
                    // Fall back to the implements list — `-Node` inside an
                    // object block means "stop implementing Node".
                    match &mut target.interfaces {
                        Some(ifaces) if !ifaces.is_empty() => {
                            let before = ifaces.len();
                            ifaces.retain(|i| i != member);
                            ifaces.len() < before
                        }
                        _ => false,
                    }
                }
            }
        };

        if !dropped {
            let what = match target.kind {
                NodeKind::Enum => "value",
                NodeKind::Union => "member",
                _ => "field",
            };
            warnings.push(format!(
                "Cannot remove \"{}.{}\" — no such {}",
                r.type_, member, what
            ));
        }
    }

    warnings
}

// ── stage 5: Relay unwrapping ──────────────────────────────────────────────

/// Matches the Relay Cursor Connections spec: the `Node` interface,
/// `PageInfo`, and any `*Edge` / `*Connection` types that carry the canonical
/// field shapes. Name-pattern + structural checks together keep unrelated
/// types (e.g. a custom `GraphEdge`) out of the filter.
fn is_relay_boilerplate(node: &GraphNodeData) -> bool {
    let empty: Vec<GraphField> = Vec::new();
    let fields = node.fields.as_ref().unwrap_or(&empty);
    let has = |n: &str| fields.iter().any(|f| f.name == n);

    match node.kind {
        NodeKind::Interface if node.name == "Node" => {
            fields.len() == 1 && fields[0].name == "id" && fields[0].type_ == "ID!"
        }
        NodeKind::Object if node.name == "PageInfo" => has("hasNextPage") && has("hasPreviousPage"),
        NodeKind::Object if node.name.ends_with("Edge") => has("node") && has("cursor"),
        NodeKind::Object if node.name.ends_with("Connection") => has("edges") && has("pageInfo"),
        _ => false,
    }
}

/// Resolves Relay Connection/Edge types to their underlying node type.
///
/// Two passes — Edges first, then Connections (which resolve through their
/// `edges` field's Edge type) — so a single lookup unwraps a Connection
/// straight to the node it carries. The displayed `type_` string is left
/// intact so the schema's actual shape stays readable.
fn unwrap_relay(nodes: &mut [GraphNodeData]) {
    let mut unwrap: HashMap<String, String> = HashMap::new();

    for n in nodes.iter() {
        if n.kind != NodeKind::Object || !n.name.ends_with("Edge") {
            continue;
        }
        let Some(fields) = &n.fields else { continue };
        let has = |name: &str| fields.iter().any(|f| f.name == name);
        if !(has("node") && has("cursor")) {
            continue;
        }
        if let Some(node_field) = fields.iter().find(|f| f.name == "node") {
            unwrap.insert(n.name.clone(), node_field.type_name.clone());
        }
    }

    for n in nodes.iter() {
        if n.kind != NodeKind::Object || !n.name.ends_with("Connection") {
            continue;
        }
        let Some(fields) = &n.fields else { continue };
        let has = |name: &str| fields.iter().any(|f| f.name == name);
        if !(has("edges") && has("pageInfo")) {
            continue;
        }
        let Some(edges_field) = fields.iter().find(|f| f.name == "edges") else { continue };
        if let Some(target) = unwrap.get(&edges_field.type_name).cloned() {
            unwrap.insert(n.name.clone(), target);
        }
    }

    for n in nodes.iter_mut() {
        let Some(fields) = &mut n.fields else { continue };
        for f in fields.iter_mut() {
            if let Some(target) = unwrap.get(&f.type_name) {
                f.is_relay_connection = true;
                f.type_name = target.clone();
            }
        }
    }
}

// ── stage 6: edge building ─────────────────────────────────────────────────

fn build_edges(nodes: &[GraphNodeData]) -> Vec<GraphEdgeData> {
    let kind_by_id: HashMap<&str, NodeKind> =
        nodes.iter().map(|n| (n.id.as_str(), n.kind)).collect();

    let mut edges: Vec<GraphEdgeData> = Vec::new();

    for n in nodes {
        if let Some(fields) = &n.fields {
            for (fi, field) in fields.iter().enumerate() {
                // Self-loops carry no information on the canvas.
                if field.type_name == n.name {
                    continue;
                }
                let args: &[GraphArg] = field.args.as_deref().unwrap_or(&[]);

                // Input-typed args, in declaration order (not deduped — the TS
                // keeps repeats so the chain mirrors the signature).
                let input_args: Vec<&str> = args
                    .iter()
                    .map(|a| a.type_name.as_str())
                    .filter(|tn| {
                        !is_builtin_scalar(tn) && kind_by_id.get(tn) == Some(&NodeKind::Input)
                    })
                    .collect();

                // Enum-typed args. Enums are leaf types, so they don't belong
                // in the navigation chain — but they still need an edge so
                // reachability doesn't misclassify them as orphans.
                let mut enum_args: Vec<&str> = Vec::new();
                {
                    let mut seen: HashSet<&str> = HashSet::new();
                    for a in args {
                        let tn = a.type_name.as_str();
                        if is_builtin_scalar(tn) || kind_by_id.get(tn) != Some(&NodeKind::Enum) {
                            continue;
                        }
                        if seen.insert(tn) {
                            enum_args.push(tn);
                        }
                    }
                }

                let return_is_scalar = is_builtin_scalar(&field.type_name);

                // Nothing graph-relevant at all.
                if return_is_scalar && input_args.is_empty() && enum_args.is_empty() {
                    continue;
                }

                for tn in &enum_args {
                    edges.push(GraphEdgeData::new(
                        format!("{}.{}:enumArg->{}", n.name, field.name, tn),
                        &n.name,
                        *tn,
                        EdgeKind::Arg,
                    ));
                }

                if !input_args.is_empty() {
                    // Chain: parent → input0 → … → returnType. A scalar return
                    // carries no graph node, so it's omitted from the chain to
                    // avoid dangling targets.
                    let mut chain: Vec<&str> = input_args.clone();
                    if !return_is_scalar {
                        chain.push(field.type_name.as_str());
                    }
                    for (ci, tgt) in chain.iter().enumerate() {
                        let src = if ci == 0 { n.name.as_str() } else { chain[ci - 1] };
                        let id = if ci == 0 {
                            format!("{}.{}->{}", n.name, field.name, tgt)
                        } else {
                            format!("{}.{}:chain{}->{}", n.name, field.name, ci, tgt)
                        };
                        let mut e = GraphEdgeData::new(
                            id,
                            src,
                            *tgt,
                            if ci == 0 { EdgeKind::Field } else { EdgeKind::Arg },
                        );
                        if ci == 0 {
                            e.source_field = Some(field.name.clone());
                            e.source_field_index = Some(fi);
                            e.label = Some(field.name.clone());
                            e.nullable = Some(field.nullable);
                        } else {
                            e.nullable = Some(false);
                        }
                        edges.push(e);
                    }
                } else if !return_is_scalar {
                    let mut e = GraphEdgeData::new(
                        format!("{}.{}->{}", n.name, field.name, field.type_name),
                        &n.name,
                        &field.type_name,
                        EdgeKind::Field,
                    );
                    e.source_field = Some(field.name.clone());
                    e.source_field_index = Some(fi);
                    e.label = Some(field.name.clone());
                    e.nullable = Some(field.nullable);
                    edges.push(e);
                }
            }
        }

        if let Some(interfaces) = &n.interfaces {
            for i in interfaces {
                // Direction: Interface → ConcreteType. Reads as "this
                // interface is implemented by these types", which makes the
                // interface the rank parent in a layered layout. Reachability
                // is preserved by the reverse-adjacency lookup in `reachable`.
                let mut e = GraphEdgeData::new(
                    format!("{}-impl-{}", i, n.name),
                    i.as_str(),
                    &n.name,
                    EdgeKind::Implements,
                );
                e.label = Some("implements".to_string());
                edges.push(e);
            }
        }

        if n.kind == NodeKind::Union {
            if let Some(members) = &n.members {
                for m in members {
                    let mut e = GraphEdgeData::new(
                        format!("{}-union-{}", n.name, m),
                        &n.name,
                        m.as_str(),
                        EdgeKind::Union,
                    );
                    e.label = Some("member".to_string());
                    edges.push(e);
                }
            }
        }
    }

    edges
}

// ── stage 7: union membership reverse index ────────────────────────────────

/// Each Object member gets the list of Unions that include it, mirroring how
/// `interfaces` hangs off the implementing type. Computed over the kept nodes
/// so a filtered-out union never shows up as a phantom membership.
fn index_union_membership(nodes: &mut [GraphNodeData]) {
    let mut unions_by_member: HashMap<&str, Vec<String>> = HashMap::new();
    for n in nodes.iter() {
        if n.kind != NodeKind::Union {
            continue;
        }
        let Some(members) = &n.members else { continue };
        for m in members {
            unions_by_member.entry(m.as_str()).or_default().push(n.name.clone());
        }
    }
    // Re-key by owned String so the mutable pass below can borrow `nodes`.
    let unions_by_member: HashMap<String, Vec<String>> =
        unions_by_member.into_iter().map(|(k, v)| (k.to_string(), v)).collect();

    for n in nodes.iter_mut() {
        if n.kind != NodeKind::Object {
            continue;
        }
        if let Some(unions) = unions_by_member.get(&n.name) {
            let mut unions = unions.clone();
            unions.sort();
            n.member_of_unions = Some(unions);
        }
    }
}

// ── stage 8: root types ────────────────────────────────────────────────────

/// Determines the root operation type names. An explicit `schema { … }`
/// definition (or `extend schema { … }`) wins outright: only the operations
/// it lists become roots. With no schema definition, GraphQL's default naming
/// applies — `Query` / `Mutation` / `Subscription` are roots iff a type with
/// that exact name exists.
fn resolve_root_types(doc: &ServiceDocument, node_names: &HashSet<&str>) -> RootTypeMap {
    let mut map = RootTypeMap::default();
    let mut explicit = false;

    for def in &doc.definitions {
        let TypeSystemDefinition::Schema(s) = def else { continue };
        if let Some(q) = &s.node.query {
            explicit = true;
            map.query = Some(q.node.as_str().to_string());
        }
        if let Some(m) = &s.node.mutation {
            explicit = true;
            map.mutation = Some(m.node.as_str().to_string());
        }
        if let Some(sub) = &s.node.subscription {
            explicit = true;
            map.subscription = Some(sub.node.as_str().to_string());
        }
    }

    if !explicit {
        if node_names.contains("Query") {
            map.query = Some("Query".to_string());
        }
        if node_names.contains("Mutation") {
            map.mutation = Some("Mutation".to_string());
        }
        if node_names.contains("Subscription") {
            map.subscription = Some("Subscription".to_string());
        }
    }
    map
}
