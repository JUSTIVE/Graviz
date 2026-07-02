import { createContext, useCallback, useContext, useMemo, useState } from "react";
import { allReachableIds, reachableFrom } from "./reachable";
import { sdlToGraph, type GraphEdgeData, type GraphNodeData, type ParsedGraph } from "./sdl-to-graph";

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
  setSchema: (input: { sdl: string; name?: string }) => void;
  clearSchema: () => void;

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

const EMPTY: ParsedGraph = { nodes: [], edges: [], error: null, warnings: [] };
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

  const graph = useMemo(
    () => (sdl ? sdlToGraph(sdl, { hideRelayBoilerplate }) : EMPTY),
    [sdl, hideRelayBoilerplate],
  );

  const effectiveRoot = useMemo(() => {
    if (rootType && graph.nodes.some((n) => n.id === rootType)) return rootType;
    const candidates = ["Query", "Mutation", "Subscription"];
    const found = candidates.find((c) => graph.nodes.some((n) => n.id === c));
    if (found) return found;
    return graph.nodes[0]?.id ?? null;
  }, [graph, rootType]);

  const visible = useMemo(() => {
    if (!effectiveRoot) return { nodes: graph.nodes, edges: graph.edges };
    return reachableFrom(graph.nodes, graph.edges, effectiveRoot);
  }, [graph, effectiveRoot]);

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
    const reachableIds = allReachableIds(graph.nodes, graph.edges);
    if (reachableIds.size === 0 && effectiveRoot) {
      const { nodes: r } = reachableFrom(graph.nodes, graph.edges, effectiveRoot);
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
  }, [graph.nodes, graph.edges, effectiveRoot, hidePrimitiveFields]);

  const setSchema = useCallback(
    ({ sdl: nextSdl, name: nextName }: { sdl: string; name?: string }) => {
      const n = nextName?.trim() || "Untitled schema";
      setSdl(nextSdl);
      setName(n);
      setFocusStack([]);
      setRootTypeState(null);
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
    try {
      sessionStorage.removeItem(STORAGE_KEY);
    } catch {
      // ignore
    }
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
      setSchema,
      clearSchema,
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
      setSchema,
      clearSchema,
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
