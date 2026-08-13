import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";
import { markOverlay, prepareOverlay, type OverlayDiff } from "./overlay";
import { allReachableIds, reachableFrom } from "./reachable";
import { sdlToGraph, type GraphEdgeData, type GraphNodeData, type NodeKind, type ParsedGraph } from "./sdl-to-graph";
import { isUntilExpired } from "./until";

/** A deprecated field (or enum value). May carry a `[until …]` sunset
 *  date (expired or upcoming), or none at all (undated deprecation). */
export interface UntilField {
  typeId: string;
  typeName: string;
  typeKind: NodeKind;
  /** Field name, or enum value name for enum members. */
  fieldName: string;
  /** Index of the field/value within its owning type's list. */
  fieldIndex: number;
  /** Sunset date (`YYYY-MM-DD`), when the deprecation carries one. */
  until?: string;
  deprecationReason?: string;
}

interface SchemaContextValue {
  sdl: string;
  name: string;
  graph: ParsedGraph;
  hasSchema: boolean;
  /** Nodes reachable from the current root type (with active filters applied). */
  visibleNodes: GraphNodeData[];
  /** Edges reachable from the current root type. */
  visibleEdges: GraphEdgeData[];
  /** Nodes with no path from the current root type. */
  orphanedNodes: GraphNodeData[];
  /** Edges whose both endpoints are orphaned. */
  orphanedEdges: GraphEdgeData[];
  /** Fields/enum values across the whole schema whose `[until …]`
   *  sunset date has already passed. Empty when none are overdue. */
  expiredFields: UntilField[];
  /** Fields/enum values carrying a `[until …]` date still in the
   *  future. Empty when none are upcoming. */
  upcomingFields: UntilField[];
  /** Deprecated fields/enum values that carry no `[until …]` date. */
  deprecatedFields: UntilField[];
  setSchema: (input: { sdl: string; name?: string }) => void;
  clearSchema: () => void;

  /** SDL currently laid over the loaded schema. Empty when none is
   *  applied. Deliberately not persisted — an overlay is a scratch
   *  experiment that should not survive a reload. */
  overlaySdl: string;
  /** Applies the given SDL on top of the loaded schema. A type the
   *  schema already declares is augmented rather than redeclared, a
   *  restated field overrides the existing one, and a `-name` line
   *  removes that field — see `lib/overlay.ts`. */
  applyOverlay: (sdl: string) => void;
  /** Drops the applied overlay, restoring the pristine schema. */
  clearOverlay: () => void;
  /** Parse error from the merged document, when the overlay makes it
   *  unparseable. The graph keeps showing the base schema in that case. */
  overlayError: string | null;
  /** What the applied overlay changed — empty when none applied. */
  overlayDiff: OverlayDiff;
  /** Problems that are the overlay's own: a `-name` that resolved to
   *  nothing, an extension of an unknown type, and the like. Warnings the
   *  base schema already had are not repeated here. */
  overlayWarnings: string[];

  rootType: string | null;
  setRootType: (id: string) => void;
  focusStack: string[];
  pushFocus: (id: string) => void;
  popTo: (index: number) => void;

  hidePrimitiveFields: boolean;
  setHidePrimitiveFields: (v: boolean) => void;
  hideRelayBoilerplate: boolean;
  setHideRelayBoilerplate: (v: boolean) => void;

  /** Field row pinned via tree-panel click. The canvas overlays an
   *  orange highlight on this exact row so the user can see which
   *  field they were inspecting on the source node, even after
   *  navigating to the field's target type. */
  pinnedField: { typeId: string; fieldName: string; fieldIndex: number } | null;
  setPinnedField: (v: { typeId: string; fieldName: string; fieldIndex: number } | null) => void;
}

const EMPTY: ParsedGraph = {
  nodes: [],
  edges: [],
  error: null,
  warnings: [],
  rootTypes: { query: null, mutation: null, subscription: null },
};
const EMPTY_DIFF: OverlayDiff = {
  addedTypes: [],
  addedFields: [],
  changedFields: [],
  removedTypes: [],
  removedFields: [],
};
const EMPTY_WARNINGS: string[] = [];
const STORAGE_KEY = "gompassql:current";
const BUILTIN_SCALARS = new Set(["String", "Int", "Float", "Boolean", "ID"]);

const SchemaContext = createContext<SchemaContextValue | null>(null);

interface Persisted {
  sdl: string;
  name: string;
}

function loadInitial(): Persisted {
  if (typeof window === "undefined") return { sdl: "", name: "" };
  try {
    const raw = sessionStorage.getItem(STORAGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Persisted;
      return { sdl: parsed.sdl ?? "", name: parsed.name ?? "" };
    }
  } catch {
    // ignore
  }
  return { sdl: "", name: "" };
}

export function SchemaProvider({ children }: { children: React.ReactNode }) {
  const initial = useMemo(loadInitial, []);
  const [sdl, setSdl] = useState(initial.sdl);
  const [name, setName] = useState(initial.name);
  const [rootType, setRootTypeState] = useState<string | null>(null);
  const [focusStack, setFocusStack] = useState<string[]>([]);
  const [hidePrimitiveFields, setHidePrimitiveFields] = useState(false);
  const [hideRelayBoilerplate, setHideRelayBoilerplate] = useState(true);
  const [pinnedField, setPinnedField] = useState<
    SchemaContextValue["pinnedField"]
  >(null);
  const [overlaySdl, setOverlaySdl] = useState("");

  const baseGraph = useMemo(
    () => (sdl ? sdlToGraph(sdl, { hideRelayBoilerplate }) : EMPTY),
    [sdl, hideRelayBoilerplate],
  );

  // With an overlay applied, the graph is parsed from base+overlay
  // (with the overlay rewritten so redeclared types augment and `-name`
  // lines subtract — see `prepareOverlay`), then diffed against the base
  // so the changes can be painted distinctly. A merged document that
  // fails to parse leaves the base graph on screen and surfaces the
  // error instead — the user keeps their view while they fix the snippet.
  //
  // Warnings are reported as the overlay's own only when the base schema
  // doesn't already produce them: the base is a prefix of the merged
  // document, so its warnings come through verbatim and would otherwise
  // read as the overlay's fault.
  const { graph, overlayError, overlayDiff, overlayWarnings } = useMemo(() => {
    if (!sdl || !overlaySdl.trim()) {
      return {
        graph: baseGraph,
        overlayError: null,
        overlayDiff: EMPTY_DIFF,
        overlayWarnings: EMPTY_WARNINGS,
      };
    }
    const prepared = prepareOverlay(sdl, overlaySdl);
    const merged = sdlToGraph(prepared.sdl, {
      hideRelayBoilerplate,
      remove: prepared.removals,
      // Restating an existing field is how the overlay changes it.
      overrideDuplicates: true,
    });
    if (merged.error) {
      return {
        graph: baseGraph,
        overlayError: merged.error,
        overlayDiff: EMPTY_DIFF,
        overlayWarnings: prepared.warnings,
      };
    }
    const fromBase = new Set(baseGraph.warnings);
    const marked = markOverlay(baseGraph, merged);
    return {
      graph: marked.graph,
      overlayError: null,
      overlayDiff: marked.diff,
      overlayWarnings: [
        ...prepared.warnings,
        ...merged.warnings.filter((w) => !fromBase.has(w)),
      ],
    };
  }, [sdl, overlaySdl, hideRelayBoilerplate, baseGraph]);

  // An overlay can take a type away, which would leave the focus trail
  // and the pinned field pointing at nodes the graph no longer has — the
  // canvas dims everything when it can't find its focus target. Prune
  // them whenever the graph changes.
  useEffect(() => {
    const ids = new Set(graph.nodes.map((n) => n.id));
    setFocusStack((prev) => {
      const next = prev.filter((id) => ids.has(id));
      return next.length === prev.length ? prev : next;
    });
    // Removing a row also shifts the ones after it, so the pin's index is
    // re-derived rather than merely validated.
    setPinnedField((prev) => {
      if (!prev) return prev;
      const node = graph.nodes.find((n) => n.id === prev.typeId);
      const rows = node?.kind === "Enum" ? node.values : node?.fields;
      const idx = rows?.findIndex((r) => r.name === prev.fieldName) ?? -1;
      if (idx < 0) return null;
      return idx === prev.fieldIndex ? prev : { ...prev, fieldIndex: idx };
    });
  }, [graph]);

  // The schema's resolved root operation types, honoring any
  // `schema { query: QueryRoot }` override. Ordered query → mutation →
  // subscription and filtered to roots that actually exist as nodes.
  const rootCandidates = useMemo(() => {
    const { query, mutation, subscription } = graph.rootTypes;
    return [query, mutation, subscription].filter(
      (c): c is string => c != null && graph.nodes.some((n) => n.id === c),
    );
  }, [graph]);

  const rootOps = useMemo(() => new Set(rootCandidates), [rootCandidates]);

  const effectiveRoot = useMemo(() => {
    if (rootType && graph.nodes.some((n) => n.id === rootType)) return rootType;
    if (rootCandidates.length > 0) return rootCandidates[0]!;
    return graph.nodes[0]?.id ?? null;
  }, [graph, rootType, rootCandidates]);

  const visible = useMemo(() => {
    if (!effectiveRoot) return { nodes: graph.nodes, edges: graph.edges };
    return reachableFrom(graph.nodes, graph.edges, effectiveRoot, rootOps);
  }, [graph, effectiveRoot, rootOps]);

  const { visibleNodes, visibleEdges } = useMemo(() => {
    if (!hidePrimitiveFields) {
      return { visibleNodes: visible.nodes, visibleEdges: visible.edges };
    }

    // Build per-node old-index → new-index maps for fields that survive the filter.
    const indexRemap = new Map<string, Map<number, number>>();
    const nodes = visible.nodes.map((n) => {
      if (!n.fields) return n;
      let newIdx = 0;
      const remap = new Map<number, number>();
      const fields = n.fields.filter((f, oldIdx) => {
        if (BUILTIN_SCALARS.has(f.typeName)) return false;
        remap.set(oldIdx, newIdx++);
        return true;
      });
      if (fields.length === n.fields.length) return n;
      indexRemap.set(n.id, remap);
      return { ...n, fields };
    });

    // Remap sourceFieldIndex on edges whose source node had fields removed.
    const edges = visible.edges.map((e) => {
      if (e.sourceFieldIndex == null) return e;
      const remap = indexRemap.get(e.source);
      if (!remap) return e;
      const newIdx = remap.get(e.sourceFieldIndex);
      if (newIdx === e.sourceFieldIndex) return e;
      // newIdx undefined means the field itself was removed — keep edge as-is
      // (primitive-typed fields never produce edges, so this shouldn't happen)
      return newIdx != null ? { ...e, sourceFieldIndex: newIdx } : e;
    });

    return { visibleNodes: nodes, visibleEdges: edges };
  }, [visible.nodes, visible.edges, hidePrimitiveFields]);

  const { orphanedNodes, orphanedEdges } = useMemo(() => {
    // Use reachability from ALL root operations so that types reachable
    // from Mutation/Subscription don't appear orphaned when root=Query.
    // Falls back to effectiveRoot for schemas without standard root ops.
    const reachableIds = allReachableIds(graph.nodes, graph.edges, rootOps);
    if (reachableIds.size === 0 && effectiveRoot) {
      const { nodes: r } = reachableFrom(graph.nodes, graph.edges, effectiveRoot, rootOps);
      for (const n of r) reachableIds.add(n.id);
    }
    const rawNodes = graph.nodes.filter((n) => !reachableIds.has(n.id));
    const orphanIds = new Set(rawNodes.map((n) => n.id));
    const rawEdges = graph.edges.filter(
      (e) => orphanIds.has(e.source) && orphanIds.has(e.target),
    );

    if (!hidePrimitiveFields) return { orphanedNodes: rawNodes, orphanedEdges: rawEdges };

    const indexRemap = new Map<string, Map<number, number>>();
    const nodes = rawNodes.map((n) => {
      if (!n.fields) return n;
      let newIdx = 0;
      const remap = new Map<number, number>();
      const fields = n.fields.filter((f, oldIdx) => {
        if (BUILTIN_SCALARS.has(f.typeName)) return false;
        remap.set(oldIdx, newIdx++);
        return true;
      });
      if (fields.length === n.fields.length) return n;
      indexRemap.set(n.id, remap);
      return { ...n, fields };
    });
    const edges = rawEdges.map((e) => {
      if (e.sourceFieldIndex == null) return e;
      const remap = indexRemap.get(e.source);
      if (!remap) return e;
      const newIdx = remap.get(e.sourceFieldIndex);
      return newIdx != null ? { ...e, sourceFieldIndex: newIdx } : e;
    });
    return { orphanedNodes: nodes, orphanedEdges: edges };
  }, [graph.nodes, graph.edges, effectiveRoot, hidePrimitiveFields, rootOps]);

  // Scan the entire schema (not just the reachable subset) for fields
  // and enum values whose `[until …]` sunset date has already passed.
  // Time-dependent, but the comparison instant is fixed per graph load —
  // a session's clock drift is immaterial against day-granularity dates.
  const { expiredFields, upcomingFields, deprecatedFields } = useMemo(() => {
    const now = Date.now();
    const expired: UntilField[] = [];
    const upcoming: UntilField[] = [];
    const deprecated: UntilField[] = [];
    for (const n of graph.nodes) {
      const rows: {
        name: string;
        until?: string;
        deprecationReason?: string;
        isDeprecated?: boolean;
      }[] = n.kind === "Enum" ? (n.values ?? []) : (n.fields ?? []);
      for (let i = 0; i < rows.length; i++) {
        const r = rows[i]!;
        // Only deprecated rows are of interest; a bare `[until …]` without
        // @deprecated wouldn't be parsed onto `until` anyway.
        if (!r.until && !r.isDeprecated) continue;
        const entry: UntilField = {
          typeId: n.id,
          typeName: n.name,
          typeKind: n.kind,
          fieldName: r.name,
          fieldIndex: i,
          until: r.until,
          deprecationReason: r.deprecationReason,
        };
        if (!r.until) deprecated.push(entry);
        else if (isUntilExpired(r.until, now)) expired.push(entry);
        else upcoming.push(entry);
      }
    }
    // Dated lists sort by date (most-overdue / soonest first); undated by
    // type then field name for a stable, scannable order.
    expired.sort((a, b) => (a.until ?? "").localeCompare(b.until ?? ""));
    upcoming.sort((a, b) => (a.until ?? "").localeCompare(b.until ?? ""));
    deprecated.sort(
      (a, b) =>
        a.typeName.localeCompare(b.typeName) || a.fieldName.localeCompare(b.fieldName),
    );
    return { expiredFields: expired, upcomingFields: upcoming, deprecatedFields: deprecated };
  }, [graph]);

  const setSchema = useCallback(
    ({ sdl: nextSdl, name: nextName }: { sdl: string; name?: string }) => {
      const n = nextName?.trim() || "Untitled schema";
      setSdl(nextSdl);
      setName(n);
      setFocusStack([]);
      setRootTypeState(null);
      // An overlay is written against a specific schema; carrying it
      // into a different one would mostly produce "cannot extend
      // unknown type" noise.
      setOverlaySdl("");
      try {
        sessionStorage.setItem(
          STORAGE_KEY,
          JSON.stringify({ sdl: nextSdl, name: n }),
        );
      } catch {
        // ignore
      }
    },
    [],
  );

  const clearSchema = useCallback(() => {
    setSdl("");
    setName("");
    setFocusStack([]);
    setRootTypeState(null);
    setOverlaySdl("");
    try {
      sessionStorage.removeItem(STORAGE_KEY);
    } catch {
      // ignore
    }
  }, []);

  const applyOverlay = useCallback((next: string) => {
    setOverlaySdl(next);
  }, []);

  const clearOverlay = useCallback(() => {
    setOverlaySdl("");
  }, []);

  const setRootType = useCallback((id: string) => {
    setRootTypeState(id);
    setFocusStack([]);
  }, []);

  const pushFocus = useCallback((id: string) => {
    setFocusStack((s) => (s[s.length - 1] === id ? s : [...s, id]));
  }, []);

  const popTo = useCallback((index: number) => {
    setFocusStack((s) => (index < 0 ? [] : s.slice(0, index + 1)));
  }, []);

  // Memoize the context value — without this every provider render
  // (any state change) hands out a fresh object identity, forcing all
  // consumers (TreePanel, SchemaCanvas, AppShell…) to re-render even
  // when nothing they read has changed.
  const value = useMemo<SchemaContextValue>(
    () => ({
      sdl,
      name,
      graph,
      hasSchema: graph.nodes.length > 0,
      visibleNodes,
      visibleEdges,
      orphanedNodes,
      orphanedEdges,
      expiredFields,
      upcomingFields,
      deprecatedFields,
      setSchema,
      clearSchema,
      overlaySdl,
      applyOverlay,
      clearOverlay,
      overlayError,
      overlayDiff,
      overlayWarnings,
      rootType: effectiveRoot,
      setRootType,
      focusStack,
      pushFocus,
      popTo,
      hidePrimitiveFields,
      setHidePrimitiveFields,
      hideRelayBoilerplate,
      setHideRelayBoilerplate,
      pinnedField,
      setPinnedField,
    }),
    [
      sdl,
      name,
      graph,
      visibleNodes,
      visibleEdges,
      orphanedNodes,
      orphanedEdges,
      expiredFields,
      upcomingFields,
      deprecatedFields,
      setSchema,
      clearSchema,
      overlaySdl,
      applyOverlay,
      clearOverlay,
      overlayError,
      overlayDiff,
      overlayWarnings,
      effectiveRoot,
      setRootType,
      focusStack,
      pushFocus,
      popTo,
      hidePrimitiveFields,
      hideRelayBoilerplate,
      pinnedField,
    ],
  );

  return <SchemaContext.Provider value={value}>{children}</SchemaContext.Provider>;
}

export function useSchema() {
  const ctx = useContext(SchemaContext);
  if (!ctx) throw new Error("useSchema must be used within SchemaProvider");
  return ctx;
}
