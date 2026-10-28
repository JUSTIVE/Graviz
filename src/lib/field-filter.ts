import type { GraphEdgeData, GraphNodeData } from "./sdl-to-graph";

const BUILTIN_SCALARS = new Set(["String", "Int", "Float", "Boolean", "ID"]);

export interface FieldFilterOptions {
  /** Hide fields whose return type is a builtin scalar. */
  hidePrimitiveFields: boolean;
  /** Hide fields a type declares only because an interface it implements
   *  does. The interface's own card still lists them. */
  hideInterfaceFields: boolean;
}

/**
 * Field names each type inherits from the interfaces it implements,
 * following interface → interface chains. Looked up against `allNodes`,
 * not the visible subset, so an interface outside the view still counts.
 */
function inheritedFieldNames(
  nodes: GraphNodeData[],
  allNodes: GraphNodeData[],
): Map<string, Set<string>> {
  const byId = new Map<string, GraphNodeData>();
  for (const n of allNodes) byId.set(n.id, n);
  const out = new Map<string, Set<string>>();
  for (const n of nodes) {
    if (!n.interfaces?.length) continue;
    const names = new Set<string>();
    const seen = new Set<string>();
    const stack = [...n.interfaces];
    while (stack.length > 0) {
      const id = stack.pop()!;
      if (seen.has(id)) continue;
      seen.add(id);
      const iface = byId.get(id);
      if (!iface) continue;
      for (const f of iface.fields ?? []) names.add(f.name);
      if (iface.interfaces) stack.push(...iface.interfaces);
    }
    out.set(n.id, names);
  }
  return out;
}

/**
 * Drops filtered fields from each node and keeps edges consistent:
 * `sourceFieldIndex` is remapped to the surviving row. An edge leaving an
 * inherited field is dropped — the interface card already draws it. A
 * hidden primitive field can still have one (`del(input: X): Boolean`
 * points at `X`), and that edge stays, anchored to the card instead of a row.
 */
export function filterFields(
  nodes: GraphNodeData[],
  edges: GraphEdgeData[],
  allNodes: GraphNodeData[],
  { hidePrimitiveFields, hideInterfaceFields }: FieldFilterOptions,
): { nodes: GraphNodeData[]; edges: GraphEdgeData[] } {
  if (!hidePrimitiveFields && !hideInterfaceFields) return { nodes, edges };

  const inherited = hideInterfaceFields
    ? inheritedFieldNames(nodes, allNodes)
    : new Map<string, Set<string>>();

  // Per-node old-index → new-index maps for fields that survive the filter.
  const indexRemap = new Map<string, Map<number, number>>();
  // Per-node old indices of fields hidden because they are inherited.
  const inheritedIdx = new Map<string, Set<number>>();
  const outNodes = nodes.map((n) => {
    if (!n.fields) return n;
    const fromIfaces = inherited.get(n.id);
    let newIdx = 0;
    const remap = new Map<number, number>();
    const dropped = new Set<number>();
    const fields = n.fields.filter((f, oldIdx) => {
      if (fromIfaces?.has(f.name)) {
        dropped.add(oldIdx);
        return false;
      }
      if (hidePrimitiveFields && BUILTIN_SCALARS.has(f.typeName)) return false;
      remap.set(oldIdx, newIdx++);
      return true;
    });
    if (fields.length === n.fields.length) return n;
    indexRemap.set(n.id, remap);
    if (dropped.size > 0) inheritedIdx.set(n.id, dropped);
    return { ...n, fields };
  });

  const outEdges: GraphEdgeData[] = [];
  for (const e of edges) {
    const remap = e.sourceFieldIndex == null ? undefined : indexRemap.get(e.source);
    if (!remap) {
      outEdges.push(e);
      continue;
    }
    const newIdx = remap.get(e.sourceFieldIndex!);
    if (newIdx == null) {
      if (inheritedIdx.get(e.source)?.has(e.sourceFieldIndex!)) continue;
      outEdges.push({ ...e, sourceFieldIndex: undefined });
      continue;
    }
    outEdges.push(newIdx === e.sourceFieldIndex ? e : { ...e, sourceFieldIndex: newIdx });
  }
  return { nodes: outNodes, edges: outEdges };
}
