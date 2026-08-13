import {
  getLocation,
  Kind,
  parse,
  type ConstDirectiveNode,
  type DefinitionNode,
  type EnumTypeExtensionNode,
  type EnumValueDefinitionNode,
  type FieldDefinitionNode,
  type InputObjectTypeExtensionNode,
  type InputValueDefinitionNode,
  type InterfaceTypeExtensionNode,
  type ObjectTypeExtensionNode,
  type TypeExtensionNode,
  type TypeNode,
  type UnionTypeExtensionNode,
} from "graphql";
import { parseUntil } from "./until";

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
  /** Sunset date (`YYYY-MM-DD`) parsed from a `[until …]` marker in the
   *  deprecation reason, when present. Used to flag overdue fields. */
  until?: string;
  /** Set when this field came from the temporary overlay SDL rather
   *  than the loaded schema. Painted with an accent marker so the
   *  addition is obvious against the base schema. */
  isOverlay?: boolean;
}

export interface EnumValue {
  name: string;
  description?: string;
  isDeprecated?: boolean;
  deprecationReason?: string;
  /** See {@link GraphField.until}. */
  until?: string;
  /** See {@link GraphField.isOverlay}. */
  isOverlay?: boolean;
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
  /** For Object types: names of Union types that include this type as
   *  a member (reverse of {@link members}). Rendered as an extra card
   *  section, like `interfaces`. */
  memberOfUnions?: string[];
  /** Set when the whole type is new in the overlay (it has no
   *  counterpart in the loaded schema). Types that merely gained
   *  overlay fields are not flagged here — their fields are. */
  isOverlay?: boolean;
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
  /** Set on a bundled edge that collapses several field edges sharing
   *  the same source→target into one arrow. Carries the names of every
   *  field that references the target, surfaced together on hover. */
  bundledLabels?: string[];
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

/**
 * A member — or a whole type — to delete from the graph once every
 * `extend` block has been folded in. This is how the overlay's `-name`
 * lines are expressed (see `overlay.ts`): SDL has no syntax for taking
 * something away, so subtraction is passed alongside the document
 * instead of inside it.
 */
export interface SchemaRemoval {
  /** Name of the type the removal targets. */
  type: string;
  /** Field, enum value, union member or implemented interface to drop.
   *  Omitted to drop the whole type. */
  member?: string;
}

export interface SdlToGraphOptions {
  /** When true (default), the standard Relay `Node` interface, `PageInfo`,
   *  and `*Edge` / `*Connection` types are folded away and field types are
   *  unwrapped to the underlying node. Set to false to surface them. */
  hideRelayBoilerplate?: boolean;
  /** Members to delete after extensions are applied. Runs before Relay
   *  unwrapping and edge building, so a removed field takes its edges —
   *  and any reachability it granted — with it. */
  remove?: readonly SchemaRemoval[];
  /** When true, a field or enum value an extension re-declares replaces
   *  the existing one (in place, keeping its row position) instead of
   *  being skipped with a duplicate warning. This is what lets the
   *  overlay change an existing field by simply restating it; a plain
   *  schema keeps the stricter reading, where a duplicate is a mistake. */
  overrideDuplicates?: boolean;
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

function parseDeprecated(directives: readonly ConstDirectiveNode[] | undefined): { isDeprecated: boolean; deprecationReason?: string; until?: string } {
  const d = directives?.find((d) => d.name.value === "deprecated");
  if (!d) return { isDeprecated: false };
  const reason = d.arguments?.find((a) => a.name.value === "reason");
  const reasonValue = reason?.value.kind === Kind.STRING ? reason.value.value : undefined;
  const until = parseUntil(reasonValue) ?? undefined;
  return { isDeprecated: true, deprecationReason: reasonValue, until };
}

/** Builds the graph-level field list for an object/interface/input
 *  definition or extension. Shared so `extend type` blocks produce
 *  fields identical to the ones a plain definition would. */
function buildFields(
  defFields: readonly (FieldDefinitionNode | InputValueDefinitionNode)[] | undefined,
): GraphField[] {
  const fields: GraphField[] = [];
  for (const f of defFields ?? []) {
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
      ...(dep.isDeprecated && { isDeprecated: true, deprecationReason: dep.deprecationReason, until: dep.until }),
    });
  }
  return fields;
}

/** See {@link buildFields} — the enum-value counterpart. */
function buildEnumValues(
  defValues: readonly EnumValueDefinitionNode[] | undefined,
): EnumValue[] {
  return (defValues ?? []).map((v) => {
    const dep = parseDeprecated(v.directives);
    return {
      name: v.name.value,
      description: v.description?.value,
      ...(dep.isDeprecated && { isDeprecated: true, deprecationReason: dep.deprecationReason, until: dep.until }),
    };
  });
}

/** Node kind each extension keyword may legally extend. */
const EXTENSION_KINDS: Partial<Record<string, NodeKind>> = {
  [Kind.OBJECT_TYPE_EXTENSION]: "Object",
  [Kind.INTERFACE_TYPE_EXTENSION]: "Interface",
  [Kind.INPUT_OBJECT_TYPE_EXTENSION]: "Input",
  [Kind.ENUM_TYPE_EXTENSION]: "Enum",
  [Kind.UNION_TYPE_EXTENSION]: "Union",
  [Kind.SCALAR_TYPE_EXTENSION]: "Scalar",
};

const EXTENSION_LABELS: Partial<Record<NodeKind, string>> = {
  Object: "type",
  Interface: "interface",
  Input: "input",
  Enum: "enum",
  Union: "union",
  Scalar: "scalar",
};

/**
 * Folds every `extend type|interface|input|enum|union|scalar` block into
 * the node it targets, mutating `nodes` in place. Runs before Relay
 * unwrapping and edge building so extended members are indistinguishable
 * from ones declared inline.
 *
 * This is what makes the overlay editor useful: an overlay saying
 * `extend type Query { report: Report }` grows the existing Query node
 * instead of colliding with it.
 *
 * Anything that can't be applied — extending a type that doesn't exist,
 * or a kind mismatch (`extend type` against an interface) — is reported
 * as a warning and skipped rather than throwing, so a half-written
 * overlay still renders the parts that do resolve.
 *
 * With `override` set, re-declaring an existing field or enum value
 * replaces it in place instead of being refused as a duplicate — see
 * {@link SdlToGraphOptions.overrideDuplicates}.
 */
function applyExtensions(
  definitions: readonly DefinitionNode[],
  nodes: GraphNodeData[],
  override: boolean,
): string[] {
  const warnings: string[] = [];
  const byName = new Map<string, GraphNodeData>();
  for (const n of nodes) byName.set(n.name, n);

  for (const def of definitions) {
    const expectedKind = EXTENSION_KINDS[def.kind];
    if (!expectedKind) continue;
    const ext = def as TypeExtensionNode;
    const name = ext.name.value;
    const label = EXTENSION_LABELS[expectedKind] ?? "type";
    const where = ext.loc
      ? (() => {
          const p = getLocation(ext.loc!.source, ext.loc!.start);
          return ` at ${p.line}:${p.column}`;
        })()
      : "";

    const target = byName.get(name);
    if (!target) {
      warnings.push(`Cannot extend unknown ${label} "${name}"${where}`);
      continue;
    }
    if (target.kind !== expectedKind) {
      warnings.push(
        `Cannot extend "${name}" as ${label}${where} — it is declared as ${EXTENSION_LABELS[target.kind] ?? target.kind}`,
      );
      continue;
    }

    if (expectedKind === "Enum") {
      const values = [...(target.values ?? [])];
      const at = new Map(values.map((v, i) => [v.name, i] as const));
      for (const v of buildEnumValues((ext as EnumTypeExtensionNode).values)) {
        const idx = at.get(v.name);
        if (idx == null) {
          at.set(v.name, values.length);
          values.push(v);
        } else if (override) {
          values[idx] = v;
        } else {
          warnings.push(`Duplicate value "${name}.${v.name}" in extension${where}`);
        }
      }
      target.values = values;
      continue;
    }

    if (expectedKind === "Union") {
      const existing = new Set(target.members ?? []);
      const added = ((ext as UnionTypeExtensionNode).types ?? [])
        .map((t) => t.name.value)
        .filter((m) => !existing.has(m) && (existing.add(m), true));
      if (added.length > 0) target.members = [...(target.members ?? []), ...added];
      continue;
    }

    // Scalar extensions only ever carry directives — nothing to merge
    // into the graph, but the target existing means it's still valid.
    if (expectedKind === "Scalar") continue;

    const fieldExt = ext as
      | ObjectTypeExtensionNode
      | InterfaceTypeExtensionNode
      | InputObjectTypeExtensionNode;
    const fields = [...(target.fields ?? [])];
    const at = new Map(fields.map((f, i) => [f.name, i] as const));
    for (const f of buildFields(fieldExt.fields)) {
      const idx = at.get(f.name);
      if (idx == null) {
        at.set(f.name, fields.length);
        fields.push(f);
      } else if (override) {
        // In place, so an overridden field keeps its row position —
        // the card doesn't reshuffle just because a type changed. A
        // block that states the same field twice lands here too, so the
        // last one written is the one that stands.
        fields[idx] = f;
      } else {
        warnings.push(`Duplicate field "${name}.${f.name}" in extension${where}`);
      }
    }
    target.fields = fields;

    const newInterfaces = (
      "interfaces" in fieldExt ? (fieldExt.interfaces ?? []) : []
    ).map((i) => i.name.value);
    if (newInterfaces.length > 0) {
      const existingIfaces = new Set(target.interfaces ?? []);
      const addedIfaces = newInterfaces.filter((i) => !existingIfaces.has(i));
      if (addedIfaces.length > 0) {
        target.interfaces = [...(target.interfaces ?? []), ...addedIfaces];
      }
    }
  }
  return warnings;
}

/**
 * Deletes the requested members from `nodes`, mutating it in place.
 * Counterpart to {@link applyExtensions}: together they let the overlay
 * both grow and shrink the loaded schema.
 *
 * A member name is looked up in whichever list the target kind renders —
 * enum values, union members, otherwise fields — falling back to the
 * implemented-interface list, so `-Node` inside an object block drops the
 * `implements Node` rather than reporting a missing field.
 *
 * As with extensions, anything that can't be resolved is warned about and
 * skipped: a half-written overlay still renders the parts that do apply.
 */
function applyRemovals(
  nodes: GraphNodeData[],
  removals: readonly SchemaRemoval[],
): string[] {
  const warnings: string[] = [];

  for (const r of removals) {
    const idx = nodes.findIndex((n) => n.name === r.type);
    if (idx < 0) {
      warnings.push(
        r.member
          ? `Cannot remove "${r.type}.${r.member}" — no type "${r.type}"`
          : `Cannot remove unknown type "${r.type}"`,
      );
      continue;
    }
    const target = nodes[idx]!;

    if (!r.member) {
      nodes.splice(idx, 1);
      continue;
    }

    const dropped = (() => {
      if (target.kind === "Enum") {
        const before = target.values?.length ?? 0;
        target.values = (target.values ?? []).filter((v) => v.name !== r.member);
        return target.values.length < before;
      }
      if (target.kind === "Union") {
        const before = target.members?.length ?? 0;
        target.members = (target.members ?? []).filter((m) => m !== r.member);
        return target.members.length < before;
      }
      const before = target.fields?.length ?? 0;
      target.fields = (target.fields ?? []).filter((f) => f.name !== r.member);
      if (target.fields.length < before) return true;
      const ifacesBefore = target.interfaces?.length ?? 0;
      if (ifacesBefore > 0) {
        target.interfaces = target.interfaces!.filter((i) => i !== r.member);
        return target.interfaces.length < ifacesBefore;
      }
      return false;
    })();

    if (!dropped) {
      const what =
        target.kind === "Enum" ? "value" : target.kind === "Union" ? "member" : "field";
      warnings.push(`Cannot remove "${r.type}.${r.member}" — no such ${what}`);
    }
  }

  return warnings;
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
  const { hideRelayBoilerplate = true, remove, overrideDuplicates = false } = options;
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

        const fields = buildFields(def.fields);

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
          values: buildEnumValues(def.values),
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

  // Fold `extend …` blocks into their targets before anything reads the
  // node list — Relay unwrapping, edge building and reachability all
  // then see the extended shape.
  const extensionWarnings = applyExtensions(doc.definitions, nodes, overrideDuplicates);

  // Subtraction comes after addition, so `-foo` can take away a field an
  // `extend` block in the same overlay just added.
  const removalWarnings =
    remove && remove.length > 0 ? applyRemovals(nodes, remove) : [];

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

  // Reverse union membership: each Object member gets the list of
  // Unions that include it, mirroring how `interfaces` hangs off the
  // implementing type. Computed over kept nodes so a filtered-out
  // union never shows up as a phantom membership.
  {
    const unionsByMember = new Map<string, string[]>();
    for (const n of keptNodes) {
      if (n.kind !== "Union" || !n.members) continue;
      for (const m of n.members) {
        const list = unionsByMember.get(m);
        if (list) list.push(n.name);
        else unionsByMember.set(m, [n.name]);
      }
    }
    for (const n of keptNodes) {
      if (n.kind !== "Object") continue;
      const unions = unionsByMember.get(n.name);
      if (unions) n.memberOfUnions = unions.sort();
    }
  }
  const keptIds = new Set(keptNodes.map((n) => n.id));
  const edges = rawEdges.filter((e) => keptIds.has(e.target) && keptIds.has(e.source));

  const rootTypes = resolveRootTypes(doc.definitions, new Set(keptNodes.map((n) => n.name)));

  return {
    nodes: keptNodes,
    edges,
    error: null,
    warnings: [...duplicateWarnings, ...extensionWarnings, ...removalWarnings],
    rootTypes,
  };
}
