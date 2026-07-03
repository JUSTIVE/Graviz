import type { GraphEdgeData, GraphNodeData } from "./sdl-to-graph";

/** Default root operation type names, used when a schema declares no
 *  explicit `schema { query: ... }` overrides. */
export const ROOT_OPS: ReadonlySet<string> = new Set(["Query", "Mutation", "Subscription"]);

export function reachableFrom(
  nodes: GraphNodeData[],
  edges: GraphEdgeData[],
  rootId: string,
  rootOps: ReadonlySet<string> = ROOT_OPS,
): { nodes: GraphNodeData[]; edges: GraphEdgeData[] } {
  if (!nodes.some((n) => n.id === rootId)) {
    return { nodes: [], edges: [] };
  }

  const excluded = new Set<string>();
  for (const r of rootOps) {
    if (r !== rootId && nodes.some((n) => n.id === r)) excluded.add(r);
  }

  // Forward adjacency (field / union / arg / implements edges). Implements
  // edges flow Interface → ConcreteType, so adj already surfaces every
  // implementor when its interface is visited.
  const adj = new Map<string, string[]>();
  // Reverse adjacency for implements: reaching a concrete type also
  // surfaces the interface(s) it implements. Without this, a BFS rooted
  // at (or arriving via) a concrete type would never traverse "up" to
  // the interface — there's no forward edge in that direction.
  const implRev = new Map<string, string[]>();

  for (const e of edges) {
    if (excluded.has(e.target)) continue;
    if (!adj.has(e.source)) adj.set(e.source, []);
    adj.get(e.source)!.push(e.target);

    if (e.kind === "implements") {
      if (!implRev.has(e.target)) implRev.set(e.target, []);
      implRev.get(e.target)!.push(e.source);
    }
  }

  const visited = new Set<string>([rootId]);
  const queue: string[] = [rootId];
  while (queue.length > 0) {
    const id = queue.shift()!;
    const next = [...(adj.get(id) ?? []), ...(implRev.get(id) ?? [])];
    for (const nb of next) {
      if (excluded.has(nb) || visited.has(nb)) continue;
      visited.add(nb);
      queue.push(nb);
    }
  }

  const visibleNodes = nodes.filter((n) => visited.has(n.id));
  const visibleEdges = edges.filter(
    (e) => visited.has(e.source) && visited.has(e.target),
  );
  return { nodes: visibleNodes, edges: visibleEdges };
}

/**
 * Returns the set of node IDs reachable from *any* root operation in the
 * schema (Query, Mutation, Subscription). Used to determine which types
 * are truly orphaned — not reachable from any entry point.
 */
export function allReachableIds(
  nodes: GraphNodeData[],
  edges: GraphEdgeData[],
  rootOps: ReadonlySet<string> = ROOT_OPS,
): Set<string> {
  const ids = new Set<string>();
  for (const root of rootOps) {
    if (!nodes.some((n) => n.id === root)) continue;
    const { nodes: reached } = reachableFrom(nodes, edges, root, rootOps);
    for (const n of reached) ids.add(n.id);
  }
  return ids;
}
