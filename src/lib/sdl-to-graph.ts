import { getLocation, Kind, parse, type ConstDirectiveNode, type TypeNode } from "graphql";

export type NodeKind = "Object" | "Interface" | "Union" | "Enum" | "Scalar" | "Input";

export interface GraphField {
  name: string;
  type: string;
  typeName: string;
  nullable: boolean;
  isRelayConnection?: boolean;
  args?: { name: string; type: string; typeName: string }[];
  description?: string;
  isDeprecated?: boolean;
  deprecationReason?: string;
}

export interface EnumValue {
  name: string;
  description?: string;
  isDeprecated?: boolean;
  deprecationReason?: string;
}

export interface GraphNodeData {
  id: string;
  name: string;
  kind: NodeKind;
  description?: string;
  fields?: GraphField[];
  values?: EnumValue[];
  members?: string[];
  interfaces?: string[];
}

export interface GraphEdgeData {
  id: string;
  source: string;
  target: string;
  sourceField?: string;
  sourceFieldIndex?: number;
  label?: string;
  kind: "field" | "implements" | "union" | "arg";
  nullable?: boolean;
}

/**
 * Resolved names of the three root operation types. By default these are
 * `Query` / `Mutation` / `Subscription`, but a `schema { query: QueryRoot }`
 * definition (or `extend schema`) can rename any of them — those overrides
 * are honored here. A slot is `null` when the schema declares no such root.
 */
export interface RootTypeMap {
  query: string | null;
  mutation: string | null;
  subscription: string | null;
}

export interface ParsedGraph {
  nodes: GraphNodeData[];
  edges: GraphEdgeData[];
  error: string | null;
  warnings: string[];
  rootTypes: RootTypeMap;
}

const EMPTY_ROOT_TYPES: RootTypeMap = { query: null, mutation: null, subscription: null };

/**
 * Determines the root operation type names. An explicit `schema { ... }`
 * definition (or `extend schema { ... }`) wins outright: only the operations
 * it lists become roots. With no schema definition, GraphQL's default naming
 * applies — `Query` / `Mutation` / `Subscription` are roots iff a type with
 * that exact name exists.
 */
function resolveRootTypes(
  definitions: readonly { kind: string }[],
  nodeNames: Set<string>,
): RootTypeMap {
  const map: RootTypeMap = { query: null, mutation: null, subscription: null };
  let explicit = false;
  for (const def of definitions) {
    if (def.kind !== Kind.SCHEMA_DEFINITION && def.kind !== Kind.SCHEMA_EXTENSION) continue;
    const operationTypes = (def as { operationTypes?: readonly { operation: string; type: { name: { value: string } } }[] }).operationTypes;
    for (const op of operationTypes ?? []) {
      explicit = true;
      const key = op.operation as keyof RootTypeMap;
      if (key in map) map[key] = op.type.name.value;
    }
  }
  if (!explicit) {
    if (nodeNames.has("Query")) map.query = "Query";
    if (nodeNames.has("Mutation")) map.mutation = "Mutation";
    if (nodeNames.has("Subscription")) map.subscription = "Subscription";
  }
  return map;
}

export interface SdlToGraphOptions {
  /** When true (default), the standard Relay `Node` interface, `PageInfo`,
   *  and `*Edge` / `*Connection` types are folded away and field types are
   *  unwrapped to the underlying node. Set to false to surface them. */
  hideRelayBoilerplate?: boolean;
}

const BUILTIN_SCALARS = new Set(["String", "Int", "Float", "Boolean", "ID"]);

/**
 * Matches the Relay Cursor Connections spec: the `Node` interface,
 * `PageInfo`, and any `*Edge` / `*Connection` types that carry the
 * canonical field shapes. Name-pattern + structural checks together
 * keep unrelated types (e.g. a custom `GraphEdge`) out of the filter.
 */
function isRelayBoilerplate(node: GraphNodeData): boolean {
  const fieldNames = new Set((node.fields ?? []).map((f) => f.name));

  if (node.kind === "Interface" && node.name === "Node") {
    const fields = node.fields ?? [];
    return (
      fields.length === 1 &&
      fields[0]!.name === "id" &&
      fields[0]!.type === "ID!"
    );
  }
  if (node.kind === "Object" && node.name === "PageInfo") {
    return fieldNames.has("hasNextPage") && fieldNames.has("hasPreviousPage");
  }
  if (node.kind === "Object" && node.name.endsWith("Edge")) {
    return fieldNames.has("node") && fieldNames.has("cursor");
  }
  if (node.kind === "Object" && node.name.endsWith("Connection")) {
    return fieldNames.has("edges") && fieldNames.has("pageInfo");
  }
  return false;
}

function parseDeprecated(directives: readonly ConstDirectiveNode[] | undefined): { isDeprecated: boolean; deprecationReason?: string } {
  const d = directives?.find((d) => d.name.value === "deprecated");
  if (!d) return { isDeprecated: false };
  const reason = d.arguments?.find((a) => a.name.value === "reason");
  const reasonValue = reason?.value.kind === Kind.STRING ? reason.value.value : undefined;
  return { isDeprecated: true, deprecationReason: reasonValue };
}

function renderType(t: TypeNode): { rendered: string; base: string } {
  if (t.kind === Kind.NON_NULL_TYPE) {
    const inner = renderType(t.type);
    return { rendered: inner.rendered + "!", base: inner.base };
  }
  if (t.kind === Kind.LIST_TYPE) {
    const inner = renderType(t.type);
    return { rendered: "[" + inner.rendered + "]", base: inner.base };
  }
  return { rendered: t.name.value, base: t.name.value };
}

export function sdlToGraph(sdl: string, options: SdlToGraphOptions = {}): ParsedGraph {
  const { hideRelayBoilerplate = true } = options;
  const nodes: GraphNodeData[] = [];

  if (!sdl.trim()) return { nodes: [], edges: [], error: null, warnings: [], rootTypes: EMPTY_ROOT_TYPES };

  let doc;
  try {
    doc = parse(sdl);
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    return { nodes: [], edges: [], error: msg, warnings: [], rootTypes: EMPTY_ROOT_TYPES };
  }

  // Detect duplicate / conflicting type-system declarations before
  // building the graph. Limited to type/interface/enum/input/union/scalar —
  // directives, schema, and `extend` blocks are skipped.
  const KIND_LABELS: Partial<Record<string, string>> = {
    [Kind.OBJECT_TYPE_DEFINITION]: "type",
    [Kind.INTERFACE_TYPE_DEFINITION]: "interface",
    [Kind.INPUT_OBJECT_TYPE_DEFINITION]: "input",
    [Kind.ENUM_TYPE_DEFINITION]: "enum",
    [Kind.UNION_TYPE_DEFINITION]: "union",
    [Kind.SCALAR_TYPE_DEFINITION]: "scalar",
  };
  const nameDecls = new Map<
    string,
    Array<{ kind: string; line: number; column: number }>
  >();
  for (const def of doc.definitions) {
    const label = KIND_LABELS[def.kind];
    if (!label || !("name" in def) || !def.name || !def.loc) continue;
    const name = def.name.value;
    const pos = getLocation(def.loc.source, def.loc.start);
    if (!nameDecls.has(name)) nameDecls.set(name, []);
    nameDecls.get(name)!.push({ kind: label, line: pos.line, column: pos.column });
  }
  const duplicateWarnings: string[] = [];
  for (const [name, decls] of nameDecls) {
    if (decls.length < 2) continue;
    const distinctKinds = new Set(decls.map((d) => d.kind));
    if (distinctKinds.size === 1) {
      const positions = decls.map((d) => `${d.line}:${d.column}`).join(", ");
      duplicateWarnings.push(`Duplicate ${decls[0]!.kind} "${name}" at ${positions}`);
    } else {
      const positions = decls
        .map((d) => `${d.kind} at ${d.line}:${d.column}`)
        .join(", ");
      duplicateWarnings.push(`Conflicting declarations for "${name}": ${positions}`);
    }
  }

  for (const def of doc.definitions) {
    switch (def.kind) {
      case Kind.OBJECT_TYPE_DEFINITION:
      case Kind.INTERFACE_TYPE_DEFINITION:
      case Kind.INPUT_OBJECT_TYPE_DEFINITION: {
        const kind: NodeKind =
          def.kind === Kind.OBJECT_TYPE_DEFINITION
            ? "Object"
            : def.kind === Kind.INTERFACE_TYPE_DEFINITION
              ? "Interface"
              : "Input";

        const fields: GraphField[] = [];
        for (const f of def.fields ?? []) {
          const t = renderType(f.type);
          const dep = parseDeprecated(f.directives);
          fields.push({
            name: f.name.value,
            type: t.rendered,
            typeName: t.base,
            nullable: f.type.kind !== Kind.NON_NULL_TYPE,
            description: f.description?.value,
            args:
              "arguments" in f && f.arguments
                ? f.arguments.map((a) => ({
                    name: a.name.value,
                    type: renderType(a.type).rendered,
                    typeName: renderType(a.type).base,
                  }))
                : undefined,
            ...(dep.isDeprecated && { isDeprecated: true, deprecationReason: dep.deprecationReason }),
          });
        }

        const interfaces =
          "interfaces" in def && def.interfaces
            ? def.interfaces.map((i) => i.name.value)
            : undefined;

        nodes.push({
          id: def.name.value,
          name: def.name.value,
          kind,
          description: def.description?.value,
          fields,
          interfaces,
        });
        break;
      }
      case Kind.ENUM_TYPE_DEFINITION:
        nodes.push({
          id: def.name.value,
          name: def.name.value,
          kind: "Enum",
          description: def.description?.value,
          values:
            def.values?.map((v) => {
              const dep = parseDeprecated(v.directives);
              return {
                name: v.name.value,
                description: v.description?.value,
                ...(dep.isDeprecated && { isDeprecated: true, deprecationReason: dep.deprecationReason }),
              };
            }) ?? [],
        });
        break;
      case Kind.UNION_TYPE_DEFINITION: {
        const members = def.types?.map((t) => t.name.value) ?? [];
        nodes.push({
          id: def.name.value,
          name: def.name.value,
          kind: "Union",
          description: def.description?.value,
          members,
        });
        break;
      }
      case Kind.SCALAR_TYPE_DEFINITION:
        nodes.push({
          id: def.name.value,
          name: def.name.value,
          kind: "Scalar",
          description: def.description?.value,
        });
        break;
      default:
        break;
    }
  }

  // Resolve Relay Connection/Edge types to their underlying node type.
  // Two passes — Edges first, then Connections (which resolve through
  // their `edges` field's Edge type) — so a single lookup unwraps a
  // Connection straight to the node it carries.
  // Only do this when boilerplate is hidden; otherwise we want fields
  // to point at the actual Connection/Edge nodes so they appear linked
  // in the graph.
  if (hideRelayBoilerplate) {
    const relayUnwrap = new Map<string, string>();
    for (const n of nodes) {
      if (n.kind !== "Object" || !n.name.endsWith("Edge")) continue;
      const fields = n.fields ?? [];
      const names = new Set(fields.map((f) => f.name));
      if (!(names.has("node") && names.has("cursor"))) continue;
      const nodeField = fields.find((f) => f.name === "node");
      if (nodeField) relayUnwrap.set(n.name, nodeField.typeName);
    }
    for (const n of nodes) {
      if (n.kind !== "Object" || !n.name.endsWith("Connection")) continue;
      const fields = n.fields ?? [];
      const names = new Set(fields.map((f) => f.name));
      if (!(names.has("edges") && names.has("pageInfo"))) continue;
      const edgesField = fields.find((f) => f.name === "edges");
      if (!edgesField) continue;
      const unwrapped = relayUnwrap.get(edgesField.typeName);
      if (unwrapped) relayUnwrap.set(n.name, unwrapped);
    }

    // Rewrite each field's target type to the unwrapped node so graph
    // edges and click-to-navigate skip the Connection/Edge wrappers.
    // The displayed `type` string is left intact so the schema's actual
    // shape is still readable in the panel and node sprites.
    for (const n of nodes) {
      if (!n.fields) continue;
      for (const f of n.fields) {
        const unwrapped = relayUnwrap.get(f.typeName);
        if (unwrapped) {
          f.isRelayConnection = true;
          f.typeName = unwrapped;
        }
      }
    }
  }

  // Now build edges from the (post-unwrap) field/interface/union data.
  const nodeKindById = new Map<string, NodeKind>();
  for (const n of nodes) nodeKindById.set(n.id, n.kind);

  const rawEdges: GraphEdgeData[] = [];
  for (const n of nodes) {
    if (n.fields) {
      for (let fi = 0; fi < n.fields.length; fi++) {
        const field = n.fields[fi]!;
        if (field.typeName === n.name) continue;

        // Collect Input-typed args (in declaration order) to build a
        // visual chain: source → input0 → input1 → … → returnType.
        const inputArgTypeNames = (field.args ?? [])
          .map((a) => a.typeName)
          .filter(
            (tn) => !BUILTIN_SCALARS.has(tn) && nodeKindById.get(tn) === "Input",
          );

        // Enum-typed args. Enums are leaf types (no fields to drill into),
        // so they don't belong in the navigation chain — but they still
        // need an edge from the source so reachability traversal doesn't
        // misclassify them as orphans when they're only referenced via
        // a field's argument list.
        const enumArgTypeNames = [
          ...new Set(
            (field.args ?? [])
              .map((a) => a.typeName)
              .filter(
                (tn) => !BUILTIN_SCALARS.has(tn) && nodeKindById.get(tn) === "Enum",
              ),
          ),
        ];

        const returnIsScalar = BUILTIN_SCALARS.has(field.typeName);

        // Skip fields with no graph-relevant target at all (scalar return,
        // no Input args, no Enum args).
        if (
          returnIsScalar &&
          inputArgTypeNames.length === 0 &&
          enumArgTypeNames.length === 0
        )
          continue;

        for (const tn of enumArgTypeNames) {
          rawEdges.push({
            id: `${n.name}.${field.name}:enumArg->${tn}`,
            source: n.name,
            target: tn,
            kind: "arg",
          });
        }

        if (inputArgTypeNames.length > 0) {
          // Chain: parent → input0 → … → returnType.
          // When the return type is a scalar it carries no graph node, so
          // omit it from the chain to avoid dangling targets.
          const chain = returnIsScalar
            ? inputArgTypeNames
            : [...inputArgTypeNames, field.typeName];
          for (let ci = 0; ci < chain.length; ci++) {
            const src = ci === 0 ? n.name : chain[ci - 1]!;
            const tgt = chain[ci]!;
            rawEdges.push({
              id:
                ci === 0
                  ? `${n.name}.${field.name}->${tgt}`
                  : `${n.name}.${field.name}:chain${ci}->${tgt}`,
              source: src,
              target: tgt,
              sourceField: ci === 0 ? field.name : undefined,
              sourceFieldIndex: ci === 0 ? fi : undefined,
              label: ci === 0 ? field.name : undefined,
              kind: ci === 0 ? "field" : "arg",
              nullable: ci === 0 ? field.nullable : false,
            });
          }
        } else if (!returnIsScalar) {
          rawEdges.push({
            id: `${n.name}.${field.name}->${field.typeName}`,
            source: n.name,
            target: field.typeName,
            sourceField: field.name,
            sourceFieldIndex: fi,
            label: field.name,
            kind: "field",
            nullable: field.nullable,
          });
        }
      }
    }
    if (n.interfaces) {
      for (const i of n.interfaces) {
        // Direction: Interface → ConcreteType. Reads as "this interface
        // is implemented by these types". Makes dot place the interface
        // as the rank parent and its implementors fan out to the right,
        // visually emphasizing the interface as a hub. Reachability is
        // preserved by the reverse-adjacency lookup in reachable.ts.
        rawEdges.push({
          id: `${i}-impl-${n.name}`,
          source: i,
          target: n.name,
          label: "implements",
          kind: "implements",
        });
      }
    }
    if (n.kind === "Union" && n.members) {
      for (const m of n.members) {
        rawEdges.push({
          id: `${n.name}-union-${m}`,
          source: n.name,
          target: m,
          label: "member",
          kind: "union",
        });
      }
    }
  }

  // Drop the Relay boilerplate nodes themselves (Node, PageInfo, and
  // every Connection/Edge we successfully unwrapped); any edges still
  // pointing at them are filtered out as dangling.
  const keptNodes = hideRelayBoilerplate
    ? nodes.filter((n) => !isRelayBoilerplate(n))
    : nodes;
  const keptIds = new Set(keptNodes.map((n) => n.id));
  const edges = rawEdges.filter((e) => keptIds.has(e.target) && keptIds.has(e.source));

  const rootTypes = resolveRootTypes(doc.definitions, new Set(keptNodes.map((n) => n.name)));

  return { nodes: keptNodes, edges, error: null, warnings: duplicateWarnings, rootTypes };
}
