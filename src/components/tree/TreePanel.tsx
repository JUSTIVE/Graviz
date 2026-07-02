import { ChevronDown, ChevronRight, Clock, Search, TriangleAlert, X } from "lucide-react";
import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { KIND_STYLES } from "@/components/graph/node-style";
import { Badge } from "@/components/ui/badge";
import { useSchema } from "@/lib/schema-context";
import type { GraphNodeData, NodeKind } from "@/lib/sdl-to-graph";
import { applyTooltipStyle, tooltipStyle } from "@/lib/tooltip-pos";
import { ColoredType } from "@/lib/type-colors";
import { cn } from "@/lib/utils";

const BUILTIN = new Set(["String", "Int", "Float", "Boolean", "ID"]);

function RelayIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 24 24" fill="#F26A03" className={className} aria-hidden>
      <path d="M2.264 4.937A2.264 2.264 0 1 0 4.456 7.77h10.339a1.792 1.792 0 0 1 0 3.583h-5.73a3.037 3.037 0 0 0-3.034 3.033a3.036 3.036 0 0 0 3.033 3.033h10.494a2.264 2.264 0 1 0 0-1.242H9.064a1.793 1.793 0 0 1-1.791-1.791c0-.988.803-1.792 1.791-1.792h5.73a3.036 3.036 0 0 0 3.034-3.033a3.036 3.036 0 0 0-3.033-3.033H4.427a2.265 2.265 0 0 0-2.163-1.592" />
    </svg>
  );
}
const ROOT_CANDIDATES = ["Query", "Mutation", "Subscription"];

// ─── Search history ────────────────────────────────────────────────────

const SEARCH_HISTORY_KEY = "graviz:search-history";
const MAX_SEARCH_HISTORY = 10;

function loadSearchHistory(): string[] {
  try {
    const raw = localStorage.getItem(SEARCH_HISTORY_KEY);
    if (!raw) return [];
    const p = JSON.parse(raw);
    return Array.isArray(p) ? p.filter((s): s is string => typeof s === "string") : [];
  } catch { return []; }
}

function saveSearchHistory(qs: string[]): void {
  try { localStorage.setItem(SEARCH_HISTORY_KEY, JSON.stringify(qs)); } catch {}
}

function pushSearchHistory(q: string, current: string[]): string[] {
  const trimmed = q.trim();
  if (!trimmed) return current;
  const next = [trimmed, ...current.filter((s) => s !== trimmed)].slice(0, MAX_SEARCH_HISTORY);
  saveSearchHistory(next);
  return next;
}

// ─── Fuzzy search ──────────────────────────────────────────────────────

function fuzzyScore(
  query: string,
  target: string,
): { score: number; indices: number[] } | null {
  if (!query) return { score: 0, indices: [] };
  const q = query.toLowerCase();
  const t = target.toLowerCase();
  let qi = 0;
  const indices: number[] = [];
  for (let i = 0; i < t.length && qi < q.length; i++) {
    if (t[i] === q[qi]) { indices.push(i); qi++; }
  }
  if (qi < q.length) return null;

  let score = 0;
  let streak = 0;
  for (let i = 0; i < indices.length; i++) {
    const idx = indices[i]!;
    const prevIdx = i > 0 ? indices[i - 1]! : -2;
    if (idx === prevIdx + 1) {
      streak++;
      score += 4 + streak * 2;
    } else {
      streak = 0;
      score += 1;
    }
    if (idx === 0) {
      score += 8;
    } else {
      const prev = target[idx - 1]!;
      const curr = target[idx]!;
      if (prev === "_" || prev === "-" || prev === ".") score += 7;
      else if (curr >= "A" && curr <= "Z") score += 5;
    }
  }
  score += Math.round((query.length / target.length) * 8);
  return { score, indices };
}

// ─── Windowed list ─────────────────────────────────────────────────────
//
// Minimal fixed-row-height virtualization for the capped (max-h-48)
// type lists. A 5k-type schema would otherwise mount 5k <li> rows the
// moment "All types" opens even though only ~8 are visible. Row height
// matches the px-3 py-1 text-xs rows (16px line + 8px padding).

const VLIST_ROW_H = 24;
const VLIST_MAX_H = 192; // == max-h-48
const VLIST_OVERSCAN = 10;

function VirtualList<T>({
  items,
  className,
  renderRow,
}: {
  items: T[];
  className?: string;
  renderRow: (item: T) => ReactNode;
}) {
  const [scrollTop, setScrollTop] = useState(0);
  const start = Math.max(0, Math.floor(scrollTop / VLIST_ROW_H) - VLIST_OVERSCAN);
  const end = Math.min(
    items.length,
    Math.ceil((scrollTop + VLIST_MAX_H) / VLIST_ROW_H) + VLIST_OVERSCAN,
  );
  return (
    <ul
      className={className}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
    >
      {start > 0 && <li style={{ height: start * VLIST_ROW_H }} aria-hidden />}
      {items.slice(start, end).map(renderRow)}
      {end < items.length && (
        <li style={{ height: (items.length - end) * VLIST_ROW_H }} aria-hidden />
      )}
    </ul>
  );
}

function HighlightedText({ text, indices, className }: { text: string; indices: number[]; className?: string }) {
  const set = new Set(indices);
  const segs: { text: string; hi: boolean }[] = [];
  let i = 0;
  while (i < text.length) {
    const hi = set.has(i);
    let j = i;
    while (j < text.length && set.has(j) === hi) j++;
    segs.push({ text: text.slice(i, j), hi });
    i = j;
  }
  return (
    <span className={className}>
      {segs.map((s, k) =>
        s.hi ? (
          <span key={k} className="font-semibold text-primary">
            {s.text}
          </span>
        ) : (
          <span key={k}>{s.text}</span>
        ),
      )}
    </span>
  );
}

export function TreePanel() {
  const {
    graph,
    visibleNodes,
    rootType,
    setRootType,
    focusStack,
    pushFocus,
    popTo,
    name,
  } = useSchema();
  const [allTypesOpen, setAllTypesOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [selectedIdx, setSelectedIdx] = useState(0);
  const [inputFocused, setInputFocused] = useState(false);
  const [searchHistory, setSearchHistory] = useState<string[]>(() => loadSearchHistory());
  const [kindFilter, setKindFilter] = useState<Set<NodeKind>>(() => new Set());
  const inputRef = useRef<HTMLInputElement>(null);
  const selectedItemRef = useRef<HTMLButtonElement>(null);

  // Cmd+K / Ctrl+K → focus search input
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  const byId = useMemo(
    () => new Map(visibleNodes.map((n) => [n.id, n])),
    [visibleNodes],
  );
  const nodesById = useMemo(
    () => new Map(graph.nodes.map((n) => [n.id, n])),
    [graph.nodes],
  );
  const roots = useMemo(
    () => ROOT_CANDIDATES.filter((r) => graph.nodes.some((n) => n.id === r)),
    [graph.nodes],
  );
  const otherRoots = useMemo(
    () =>
      graph.nodes
        .filter((n) => !ROOT_CANDIDATES.includes(n.id))
        .map((n) => n.id)
        .sort(),
    [graph.nodes],
  );

  const path: string[] = useMemo(() => {
    if (!rootType) return [];
    return [rootType, ...focusStack];
  }, [rootType, focusStack]);

  const currentId = path[path.length - 1];
  const current = currentId ? (byId.get(currentId) ?? null) : null;

  const isNavigable = (typeName: string) =>
    !BUILTIN.has(typeName) && byId.has(typeName);

  const allTypesSorted = useMemo(
    () => [...graph.nodes].sort((a, b) => a.name.localeCompare(b.name)),
    [graph.nodes],
  );

  // Types that implement the currently-selected interface. Empty when
  // the current type is not an Interface so the implementers section
  // doesn't render.
  const implementers = useMemo(() => {
    if (!current || current.kind !== "Interface") return [];
    return graph.nodes
      .filter((n) => n.interfaces?.includes(current.id))
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [current, graph.nodes]);

  // Reverse map: which unions contain each type as a member. Used to
  // annotate union-member rows with the other unions they participate
  // in.
  const unionsByMember = useMemo(() => {
    const m = new Map<string, string[]>();
    for (const n of graph.nodes) {
      if (n.kind !== "Union" || !n.members) continue;
      for (const member of n.members) {
        const list = m.get(member);
        if (list) list.push(n.id);
        else m.set(member, [n.id]);
      }
    }
    return m;
  }, [graph.nodes]);

  // Members of the currently-selected union, resolved to their full
  // node data so we can render kind badges and other-union chips.
  const unionMembers = useMemo(() => {
    if (!current || current.kind !== "Union") return [];
    return (current.members ?? [])
      .map((m) => nodesById.get(m))
      .filter((n): n is GraphNodeData => !!n)
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [current, nodesById]);

  // Types that reference the currently-selected type as a field type
  // (incoming edges). Each entry also carries the field names that
  // make the reference so users can see *how* a type is consumed.
  const referencedBy = useMemo(() => {
    if (!current) return [];
    const results: { node: GraphNodeData; fields: string[] }[] = [];
    for (const n of graph.nodes) {
      if (!n.fields) continue;
      const fields: string[] = [];
      for (const f of n.fields) {
        if (f.typeName === current.id) fields.push(f.name);
      }
      if (fields.length > 0) results.push({ node: n, fields });
    }
    return results.sort((a, b) => a.node.name.localeCompare(b.node.name));
  }, [current, graph.nodes]);

  interface SearchResult {
    typeId: string;
    typeName: string;
    typeKind: GraphNodeData["kind"];
    fieldName?: string;
    fieldType?: string;
    score: number;
    matchIndices: number[];
    typeMatchIndices?: number[]; // set when query is "Type.field" form
  }

  const searchResults = useMemo<SearchResult[]>(() => {
    const q = query.trim();
    if (!q) return [];
    const out: SearchResult[] = [];

    const dotIdx = q.indexOf(".");
    if (dotIdx > 0) {
      // "Type.field" mode: left side matches type name, right side matches field/value name.
      const typePart = q.slice(0, dotIdx);
      const fieldPart = q.slice(dotIdx + 1);
      for (const node of graph.nodes) {
        const tm = fuzzyScore(typePart, node.name);
        if (!tm) continue;
        const rowsF = node.fields ?? [];
        const rowsV = node.values ?? [];
        const rows: { name: string; type?: string }[] = [
          ...rowsF.map((f) => ({ name: f.name, type: f.type })),
          ...rowsV.map((v) => ({ name: v.name })),
        ];
        for (const row of rows) {
          const fm = fieldPart ? fuzzyScore(fieldPart, row.name) : { score: 0, indices: [] as number[] };
          if (!fm) continue;
          out.push({
            typeId: node.id,
            typeName: node.name,
            typeKind: node.kind,
            fieldName: row.name,
            fieldType: row.type,
            score: tm.score + fm.score,
            matchIndices: fm.indices,
            typeMatchIndices: tm.indices,
          });
        }
      }
    } else {
      // Plain mode: fuzzy-match query against type names, field names, and enum values.
      for (const node of graph.nodes) {
        const tm = fuzzyScore(q, node.name);
        if (tm) {
          out.push({
            typeId: node.id,
            typeName: node.name,
            typeKind: node.kind,
            score: tm.score + 3,
            matchIndices: tm.indices,
          });
        }
        for (const f of node.fields ?? []) {
          const fm = fuzzyScore(q, f.name);
          if (fm) {
            out.push({
              typeId: node.id,
              typeName: node.name,
              typeKind: node.kind,
              fieldName: f.name,
              fieldType: f.type,
              score: fm.score,
              matchIndices: fm.indices,
            });
          }
        }
        for (const v of node.values ?? []) {
          const vm = fuzzyScore(q, v.name);
          if (vm) {
            out.push({
              typeId: node.id,
              typeName: node.name,
              typeKind: node.kind,
              fieldName: v.name,
              score: vm.score,
              matchIndices: vm.indices,
            });
          }
        }
      }
    }

    out.sort((a, b) => b.score - a.score);
    return out.slice(0, 80);
  }, [query, graph.nodes]);

  const kindCounts = useMemo(() => {
    const counts = new Map<NodeKind, number>();
    for (const r of searchResults) {
      counts.set(r.typeKind, (counts.get(r.typeKind) ?? 0) + 1);
    }
    return counts;
  }, [searchResults]);

  const filteredResults = useMemo(
    () =>
      kindFilter.size === 0
        ? searchResults
        : searchResults.filter((r) => kindFilter.has(r.typeKind)),
    [searchResults, kindFilter],
  );

  const toggleKindFilter = (kind: NodeKind) => {
    setKindFilter((prev) => {
      const next = new Set(prev);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });
  };

  useEffect(() => { setSelectedIdx(0); }, [filteredResults]);

  useEffect(() => {
    selectedItemRef.current?.scrollIntoView({ block: "nearest" });
  }, [selectedIdx]);

  const saveQueryToHistory = (q: string) => {
    setSearchHistory((h) => pushSearchHistory(q, h));
  };

  const jumpToAndClose = (id: string) => {
    if (query.trim()) saveQueryToHistory(query);
    jumpTo(id);
    setQuery("");
    inputRef.current?.blur();
  };

  const onSearchKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Escape") { setQuery(""); inputRef.current?.blur(); return; }
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIdx((i) => Math.min(i + 1, filteredResults.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIdx((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      const r = filteredResults[selectedIdx];
      if (r) jumpToAndClose(r.typeId);
    }
  };

  const removeHistoryItem = (q: string) => {
    setSearchHistory((h) => {
      const next = h.filter((s) => s !== q);
      saveSearchHistory(next);
      return next;
    });
  };

  const clearAllHistory = () => {
    setSearchHistory([]);
    saveSearchHistory([]);
  };

  const jumpTo = (id: string) => {
    if (id === rootType) {
      popTo(-1);
      return;
    }
    const idx = focusStack.indexOf(id);
    if (idx >= 0) {
      popTo(idx);
      return;
    }
    // If the type is not visible in the current graph, make it the new root
    // so it appears in the canvas and can be highlighted.
    if (!byId.has(id)) {
      setRootType(id);
      return;
    }
    pushFocus(id);
  };

  return (
    <div className="flex h-full min-h-0 flex-col [&_::-webkit-scrollbar]:w-0 [&_::-webkit-scrollbar]:h-0 [scrollbar-width:none]">
      {/* Search input + filters */}
      <div className="border-b border-border px-3 py-2">
        <div className="relative flex items-center">
          <Search className="pointer-events-none absolute left-2 h-3 w-3 text-muted-foreground" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={onSearchKeyDown}
            onFocus={() => setInputFocused(true)}
            onBlur={() => setTimeout(() => setInputFocused(false), 150)}
            placeholder="Search types & fields…"
            className="w-full rounded border border-border bg-background py-1.5 pl-7 pr-6 text-xs placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
          />
          {query ? (
            <button
              type="button"
              onClick={() => { setQuery(""); inputRef.current?.focus(); }}
              className="absolute right-2 text-muted-foreground hover:text-foreground"
            >
              <X className="h-3 w-3" />
            </button>
          ) : (
            <span className="pointer-events-none absolute right-2 font-mono text-[10px] text-muted-foreground/50">
              ⌘K
            </span>
          )}
        </div>
      </div>

      {/* Recent search history (shown when focused + query empty) */}
      {inputFocused && !query.trim() && searchHistory.length > 0 && (
        <div className="min-h-0 flex-1 overflow-auto">
          <div className="flex items-center justify-between px-3 py-1.5">
            <span className="text-[10px] uppercase tracking-wider text-muted-foreground">Recent</span>
            <button
              type="button"
              onMouseDown={(e) => e.preventDefault()}
              onClick={clearAllHistory}
              className="text-[10px] text-muted-foreground hover:text-foreground"
            >
              Clear all
            </button>
          </div>
          <ul>
            {searchHistory.map((q) => (
              <li key={q} className="flex items-center">
                <button
                  type="button"
                  className="flex flex-1 items-center gap-2 px-3 py-1.5 text-left font-mono text-xs hover:bg-secondary/60"
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => setQuery(q)}
                >
                  <Clock className="h-3 w-3 shrink-0 text-muted-foreground" />
                  <span className="truncate">{q}</span>
                </button>
                <button
                  type="button"
                  className="mr-3 shrink-0 rounded p-0.5 text-muted-foreground hover:text-foreground"
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => removeHistoryItem(q)}
                >
                  <X className="h-3 w-3" />
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* Search results */}
      {query.trim() && (
        <div className="flex min-h-0 flex-1 flex-col">
          {searchResults.length > 0 && (
            <div className="flex flex-wrap items-center gap-1 border-b border-border px-3 py-1.5">
              {(Object.keys(KIND_STYLES) as NodeKind[])
                .filter((k) => (kindCounts.get(k) ?? 0) > 0)
                .map((k) => {
                  const style = KIND_STYLES[k];
                  const active = kindFilter.has(k);
                  const count = kindCounts.get(k) ?? 0;
                  return (
                    <button
                      key={k}
                      type="button"
                      onMouseDown={(e) => e.preventDefault()}
                      onClick={() => toggleKindFilter(k)}
                      className={cn(
                        "flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium transition-colors",
                        active
                          ? cn(style.header, "border-transparent")
                          : "border-border text-muted-foreground hover:border-border/80 hover:text-foreground",
                      )}
                    >
                      <span>{style.label}</span>
                      <span className={cn("font-mono text-[9px]", active ? "" : "opacity-70")}>
                        {count}
                      </span>
                    </button>
                  );
                })}
              {kindFilter.size > 0 && (
                <button
                  type="button"
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => setKindFilter(new Set())}
                  className="ml-auto flex items-center gap-1 rounded-full px-1.5 py-0.5 text-[10px] text-muted-foreground hover:text-foreground"
                >
                  <X className="h-2.5 w-2.5" />
                  Clear
                </button>
              )}
            </div>
          )}
          <div className="min-h-0 flex-1 overflow-auto">
          {filteredResults.length === 0 ? (
            <div className="p-6 text-center text-xs text-muted-foreground">No results</div>
          ) : (
            <ul>
              {filteredResults.map((r, i) => {
                const style = KIND_STYLES[r.typeKind];
                const isSelected = i === selectedIdx;
                return (
                  <li key={`${r.typeId}:${r.fieldName ?? ""}:${i}`}>
                    <button
                      ref={isSelected ? selectedItemRef : undefined}
                      type="button"
                      onClick={() => jumpToAndClose(r.typeId)}
                      onMouseEnter={() => setSelectedIdx(i)}
                      className={cn(
                        "flex w-full items-center gap-2 px-3 py-1.5 text-left font-mono text-xs transition-colors",
                        isSelected ? "bg-secondary" : "hover:bg-secondary/60",
                      )}
                    >
                      <Badge className={cn("shrink-0 px-1.5 py-0 text-[9px] leading-4", style.badge)}>
                        {style.label}
                      </Badge>
                      {r.fieldName ? (
                        <span className="min-w-0 flex-1 truncate">
                          {r.typeMatchIndices ? (
                            <HighlightedText text={r.typeName} indices={r.typeMatchIndices} className="text-muted-foreground" />
                          ) : (
                            <span className="text-muted-foreground">{r.typeName}</span>
                          )}
                          <span className="text-muted-foreground">.</span>
                          <HighlightedText text={r.fieldName} indices={r.matchIndices} />
                        </span>
                      ) : (
                        <span className="min-w-0 flex-1 truncate">
                          <HighlightedText text={r.typeName} indices={r.matchIndices} />
                        </span>
                      )}
                      {r.fieldType && (
                        <span className="shrink-0 text-[10px] text-muted-foreground">
                          {r.fieldType}
                        </span>
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          </div>
        </div>
      )}

      {/* Normal tree (hidden while searching or showing history) */}
      {!query.trim() && !(inputFocused && searchHistory.length > 0) && <>
      <div className="border-b border-border p-3">
        <div className="mb-1 text-[10px] uppercase tracking-wider text-muted-foreground">
          {name || "Schema"}
        </div>
        <div className="flex flex-wrap gap-1">
          {roots.map((r) => (
            <button
              key={r}
              onClick={() => setRootType(r)}
              className={cn(
                "rounded px-2 py-1 font-mono text-xs transition-colors",
                rootType === r
                  ? "bg-primary text-primary-foreground"
                  : "bg-secondary text-secondary-foreground hover:bg-secondary/70",
              )}
            >
              {r}
            </button>
          ))}
          {roots.length === 0 && (
            <span className="text-xs text-muted-foreground">
              No root operations; pick any type:
            </span>
          )}
        </div>
        {roots.length === 0 && otherRoots.length > 0 && (
          <select
            className="mt-2 w-full rounded border border-border bg-background px-2 py-1 text-xs"
            value={rootType ?? ""}
            onChange={(e) => setRootType(e.target.value)}
          >
            {otherRoots.map((id) => (
              <option key={id} value={id}>
                {id}
              </option>
            ))}
          </select>
        )}
      </div>

      {graph.nodes.length > 0 && (
        <div className="border-b border-border">
          <button
            type="button"
            onClick={() => setAllTypesOpen((v) => !v)}
            className="flex w-full items-center gap-1 px-3 py-2 text-[10px] uppercase tracking-wider text-muted-foreground hover:bg-secondary/40"
          >
            {allTypesOpen ? (
              <ChevronDown className="h-3 w-3" />
            ) : (
              <ChevronRight className="h-3 w-3" />
            )}
            <span>All types ({graph.nodes.length})</span>
          </button>
          {allTypesOpen && (
            <VirtualList
              items={allTypesSorted}
              className="max-h-48 overflow-auto border-t border-border"
              renderRow={(n) => {
                const selected = n.id === currentId;
                const style = KIND_STYLES[n.kind];
                return (
                  <li key={n.id}>
                    <button
                      type="button"
                      onClick={() => jumpTo(n.id)}
                      className={cn(
                        "flex w-full items-center gap-2 px-3 py-1 text-left font-mono text-xs transition-colors",
                        selected
                          ? "bg-primary text-primary-foreground"
                          : "text-foreground hover:bg-secondary/60",
                      )}
                    >
                      <Badge
                        className={cn(
                          "shrink-0 px-1.5 py-0 text-[9px] leading-4",
                          selected
                            ? "bg-primary-foreground/20 text-primary-foreground"
                            : style.badge,
                        )}
                      >
                        {style.label}
                      </Badge>
                      <span className="truncate">{n.name}</span>
                    </button>
                  </li>
                );
              }}
            />
          )}
        </div>
      )}

      {implementers.length > 0 && (
        <div className="border-b border-border">
          <div className="flex w-full items-center gap-1 px-3 py-2 text-[10px] uppercase tracking-wider text-muted-foreground">
            <span>Implemented by ({implementers.length})</span>
          </div>
          <ul className="max-h-48 overflow-auto border-t border-border">
            {implementers.map((n) => {
              const selected = n.id === currentId;
              const style = KIND_STYLES[n.kind];
              // Show the *other* interfaces this implementer also
              // implements so the user can see when one type
              // implements multiple interfaces at a glance. The
              // current interface is implicit from the section header.
              const otherIfaces = (n.interfaces ?? []).filter((i) => i !== current?.id);
              return (
                <li key={n.id}>
                  <button
                    type="button"
                    onClick={() => jumpTo(n.id)}
                    className={cn(
                      "flex w-full items-center gap-2 px-3 py-1 text-left font-mono text-xs transition-colors",
                      selected
                        ? "bg-primary text-primary-foreground"
                        : "text-foreground hover:bg-secondary/60",
                    )}
                  >
                    <Badge
                      className={cn(
                        "shrink-0 px-1.5 py-0 text-[9px] leading-4",
                        selected
                          ? "bg-primary-foreground/20 text-primary-foreground"
                          : style.badge,
                      )}
                    >
                      {style.label}
                    </Badge>
                    <span className="truncate">{n.name}</span>
                    {otherIfaces.length > 0 && (
                      <span className="ml-auto flex shrink-0 flex-wrap items-center gap-1">
                        {otherIfaces.map((i) => (
                          <span
                            key={i}
                            className={cn(
                              "rounded px-1 py-0 text-[9px] leading-4",
                              selected
                                ? "bg-primary-foreground/20 text-primary-foreground"
                                : "bg-secondary text-muted-foreground",
                            )}
                          >
                            {i}
                          </span>
                        ))}
                      </span>
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      {unionMembers.length > 0 && (
        <div className="border-b border-border">
          <div className="flex w-full items-center gap-1 px-3 py-2 text-[10px] uppercase tracking-wider text-muted-foreground">
            <span>Members ({unionMembers.length})</span>
          </div>
          <ul className="max-h-48 overflow-auto border-t border-border">
            {unionMembers.map((n) => {
              const selected = n.id === currentId;
              const style = KIND_STYLES[n.kind];
              // Show the *other* unions this member also participates
              // in. The current union is implicit from the section
              // header.
              const otherUnions = (unionsByMember.get(n.id) ?? []).filter((u) => u !== current?.id);
              return (
                <li key={n.id}>
                  <button
                    type="button"
                    onClick={() => jumpTo(n.id)}
                    className={cn(
                      "flex w-full items-center gap-2 px-3 py-1 text-left font-mono text-xs transition-colors",
                      selected
                        ? "bg-primary text-primary-foreground"
                        : "text-foreground hover:bg-secondary/60",
                    )}
                  >
                    <Badge
                      className={cn(
                        "shrink-0 px-1.5 py-0 text-[9px] leading-4",
                        selected
                          ? "bg-primary-foreground/20 text-primary-foreground"
                          : style.badge,
                      )}
                    >
                      {style.label}
                    </Badge>
                    <span className="truncate">{n.name}</span>
                    {otherUnions.length > 0 && (
                      <span className="ml-auto flex shrink-0 flex-wrap items-center gap-1">
                        {otherUnions.map((u) => (
                          <span
                            key={u}
                            className={cn(
                              "rounded px-1 py-0 text-[9px] leading-4",
                              selected
                                ? "bg-primary-foreground/20 text-primary-foreground"
                                : "bg-secondary text-muted-foreground",
                            )}
                          >
                            {u}
                          </span>
                        ))}
                      </span>
                    )}
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      {referencedBy.length > 0 && (
        <div className="border-b border-border">
          <div className="flex w-full items-center gap-1 px-3 py-2 text-[10px] uppercase tracking-wider text-muted-foreground">
            <span>Referenced by ({referencedBy.length})</span>
          </div>
          <ul className="max-h-48 overflow-auto border-t border-border">
            {referencedBy.map(({ node: n, fields }) => {
              const selected = n.id === currentId;
              const style = KIND_STYLES[n.kind];
              return (
                <li key={n.id}>
                  <button
                    type="button"
                    onClick={() => jumpTo(n.id)}
                    className={cn(
                      "flex w-full items-center gap-2 px-3 py-1 text-left font-mono text-xs transition-colors",
                      selected
                        ? "bg-primary text-primary-foreground"
                        : "text-foreground hover:bg-secondary/60",
                    )}
                  >
                    <Badge
                      className={cn(
                        "shrink-0 px-1.5 py-0 text-[9px] leading-4",
                        selected
                          ? "bg-primary-foreground/20 text-primary-foreground"
                          : style.badge,
                      )}
                    >
                      {style.label}
                    </Badge>
                    <span className="truncate">{n.name}</span>
                    <span className="ml-auto flex shrink-0 flex-wrap items-center gap-1">
                      {fields.map((f) => (
                        <span
                          key={f}
                          className={cn(
                            "rounded px-1 py-0 text-[9px] leading-4",
                            selected
                              ? "bg-primary-foreground/20 text-primary-foreground"
                              : "bg-secondary text-muted-foreground",
                          )}
                        >
                          .{f}
                        </span>
                      ))}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-auto">
        {!current ? (
          <div className="p-6 text-center text-xs text-muted-foreground">
            Select a type to start exploring.
          </div>
        ) : (
          <TypeDetail
            node={current}
            isNavigable={isNavigable}
            onNavigate={jumpTo}
            nodesById={nodesById}
          />
        )}
      </div>
      </>}
    </div>
  );
}

function TypeDetail({
  node,
  isNavigable,
  onNavigate,
  nodesById,
}: {
  node: GraphNodeData;
  isNavigable: (t: string) => boolean;
  onNavigate: (id: string) => void;
  nodesById: Map<string, GraphNodeData>;
}) {
  const { setPinnedField } = useSchema();
  const style = KIND_STYLES[node.kind];

  const chainNavigable = (typeName: string) =>
    !BUILTIN.has(typeName) && nodesById.has(typeName);

  return (
    <div className="p-3">
      <div className="mb-2 flex items-center gap-2">
        <Badge className={style.badge}>{style.label}</Badge>
        <div className="truncate font-mono text-sm font-semibold">{node.name}</div>
      </div>

      {node.description && (
        <p className="mb-3 text-xs text-muted-foreground">{node.description}</p>
      )}

      {node.interfaces && node.interfaces.length > 0 && (
        <div className="mb-3 flex flex-wrap items-center gap-1 text-[11px] text-muted-foreground">
          <span>implements</span>
          {node.interfaces.map((i) => (
            <button
              key={i}
              className={cn(
                "rounded px-1.5 py-0.5 font-mono",
                isNavigable(i)
                  ? "bg-secondary/60 hover:bg-secondary"
                  : "opacity-60",
              )}
              disabled={!isNavigable(i)}
              onClick={() => isNavigable(i) && onNavigate(i)}
            >
              {i}
            </button>
          ))}
        </div>
      )}

      {node.kind === "Enum" ? (
        <ul className="space-y-0.5 font-mono text-xs text-muted-foreground">
          {node.values?.map((v) => (
            <li key={v.name} className="rounded px-2 py-1">
              <div className="flex flex-col gap-0.5">
                <span className={cn("flex items-center gap-1", v.isDeprecated && "text-muted-foreground/60")}>
                  {v.isDeprecated && <TriangleAlert className="h-2.5 w-2.5 shrink-0 text-amber-500/70" />}
                  <span className={cn("text-foreground", v.isDeprecated && "line-through decoration-muted-foreground/40")}>{v.name}</span>
                </span>
                {v.deprecationReason && (
                  <span className="text-[11px] font-sans leading-snug text-amber-600/70 dark:text-amber-400/70">
                    {v.deprecationReason}
                  </span>
                )}
                {v.description && (
                  <span className="text-[11px] font-sans leading-snug text-muted-foreground">
                    {v.description}
                  </span>
                )}
              </div>
            </li>
          ))}
          {(!node.values || node.values.length === 0) && (
            <li className="italic">no values</li>
          )}
        </ul>
      ) : node.kind === "Union" ? (
        <ul className="space-y-0.5 font-mono text-xs">
          {node.members?.map((m) => (
            <li key={m}>
              <FieldRow
                label={`| ${m}`}
                chain={[{ label: "", typeName: m, navigable: chainNavigable(m) }]}
                onNavigate={onNavigate}
              />
            </li>
          ))}
        </ul>
      ) : node.kind === "Scalar" ? (
        <p className="text-xs italic text-muted-foreground">
          {node.description ? "" : "custom scalar"}
        </p>
      ) : (
        <ul className="space-y-0.5 font-mono text-xs">
          {node.fields?.map((f, fieldIndex) => {
            // Only show the return type in the chain — Input args are
            // visible on hover via the argsDetail list, not inline.
            const chain: ChainItem[] = [
              {
                label: f.type,
                typeName: f.typeName,
                navigable: chainNavigable(f.typeName),
                isRelayConnection: f.isRelayConnection,
              },
            ];
            return (
              <li key={f.name}>
                <FieldRow
                  label={f.name}
                  chain={chain}
                  description={f.description}
                  args={f.args?.map((a) => ({ ...a, navigable: isNavigable(a.typeName) }))}
                  isDeprecated={f.isDeprecated}
                  deprecationReason={f.deprecationReason}
                  onNavigate={onNavigate}
                  onPin={() =>
                    setPinnedField({
                      typeId: node.id,
                      fieldName: f.name,
                      fieldIndex,
                    })
                  }
                />
              </li>
            );
          })}
          {(!node.fields || node.fields.length === 0) && (
            <li className="px-2 py-1 italic text-muted-foreground">no fields</li>
          )}
        </ul>
      )}
    </div>
  );
}

interface ChainItem {
  label: string;
  typeName: string;
  navigable: boolean;
  isRelayConnection?: boolean;
}

function FieldRow({
  label,
  chain,
  description,
  args,
  isDeprecated,
  deprecationReason,
  onNavigate,
  onPin,
}: {
  label: string;
  chain: ChainItem[];
  description?: string;
  args?: { name: string; type: string; typeName: string; navigable: boolean }[];
  isDeprecated?: boolean;
  deprecationReason?: string;
  onNavigate: (id: string) => void;
  /** Called whenever the row itself is clicked — used to pin this
   *  field's highlight on the canvas. Independent of navigation:
   *  pinning happens even on non-navigable types so the user can
   *  pin a field with a scalar return. */
  onPin?: () => void;
}) {
  const [hovered, setHovered] = useState(false);
  // Tooltip position lives in a ref + direct DOM style writes so a
  // mousemove over the row doesn't re-render the row per event; only
  // hover enter/leave (which mounts/unmounts the tooltip) uses state.
  const tipPosRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const tipElRef = useRef<HTMLDivElement | null>(null);
  const moveTip = (x: number, y: number) => {
    tipPosRef.current = { x, y };
    if (tipElRef.current) applyTooltipStyle(tipElRef.current, x, y);
  };
  const requiredArgCount = args?.filter((a) => a.type.endsWith("!")).length ?? 0;
  const hasArgs = (args?.length ?? 0) > 0;

  // Custom tooltip rendered when the row is hovered. Replaces the
  // native `title` attribute so the bubble matches the rest of the
  // app's popovers (font, theme, edge-aware placement) and can show
  // colored type segments rather than plain text.
  const tooltipEl = hovered ? (
    <div
      ref={tipElRef}
      className="pointer-events-none fixed z-50 whitespace-nowrap rounded-lg border border-border bg-popover/95 px-3 py-2 font-mono text-xs text-popover-foreground shadow-lg backdrop-blur"
      style={tooltipStyle(tipPosRef.current.x, tipPosRef.current.y)}
    >
      <span className="font-semibold">{label}</span>
      {chain.length > 0 && (
        <>
          <span className="text-muted-foreground">: </span>
          {chain.map((c, i) => (
            <Fragment key={i}>
              {i > 0 && <span className="text-muted-foreground"> → </span>}
              <ColoredType type={c.label} />
            </Fragment>
          ))}
        </>
      )}
    </div>
  ) : null;
  const arityBadge = hasArgs ? (
    <span className="font-mono text-[10px] text-muted-foreground/60">
      ({requiredArgCount}/{args!.length})
    </span>
  ) : null;

  const deprecatedNote = isDeprecated ? (
    <span className="flex items-center gap-1 font-sans text-[11px] leading-snug text-amber-600/80 dark:text-amber-400/80">
      <TriangleAlert className="h-2.5 w-2.5 shrink-0" />
      {deprecationReason ?? "Deprecated"}
    </span>
  ) : null;

  const argsDetail = hovered && hasArgs ? (
    <ul className="mt-0.5 space-y-px border-l border-border pl-2">
      {args!.map((a) => (
        <li key={a.name} className="flex items-center gap-1.5 font-mono text-[10px]">
          <span className="text-muted-foreground">{a.name}:</span>
          {a.navigable ? (
            <button
              type="button"
              onClick={(e) => { e.stopPropagation(); onNavigate(a.typeName); }}
              className="flex items-center gap-0.5 rounded px-0.5 hover:bg-secondary/80"
            >
              <ColoredType type={a.type} />
              <ChevronRight className="h-2.5 w-2.5 text-muted-foreground" />
            </button>
          ) : (
            <ColoredType type={a.type} />
          )}
        </li>
      ))}
    </ul>
  ) : null;

  const typeChip = (item: ChainItem) =>
    item.navigable ? (
      <button
        type="button"
        onClick={(e) => { e.stopPropagation(); onNavigate(item.typeName); }}
        className="group/chip flex min-w-0 items-center gap-0.5 rounded px-1 ring-1 ring-transparent transition-colors hover:bg-primary/15 hover:ring-primary/40"
        title={item.label}
      >
        {item.isRelayConnection && item.label && (
          <RelayIcon className="h-2.5 w-2.5 shrink-0 opacity-80" />
        )}
        {item.label ? (
          <span className="min-w-0 truncate">
            <ColoredType type={item.label} />
          </span>
        ) : null}
        <ChevronRight className="h-3 w-3 shrink-0 text-muted-foreground transition-transform group-hover/chip:translate-x-0.5 group-hover/chip:text-primary" />
      </button>
    ) : (
      <span
        className="flex min-w-0 items-center gap-0.5"
        title={item.label}
      >
        {item.isRelayConnection && item.label && (
          <RelayIcon className="h-2.5 w-2.5 shrink-0 opacity-80" />
        )}
        {item.label ? (
          <span className="min-w-0 truncate">
            <ColoredType type={item.label} />
          </span>
        ) : null}
      </span>
    );
  // Row is a div with two clickable regions: the outer area pins the
  // field (focusing the canvas on its owner type), while the inner
  // type chip(s) navigate to the return type. Buttons in args / chips
  // stop propagation so they don't also fire the row's pin handler.
  return (
    <>
    {tooltipEl}
    <div
      className={cn("flex w-full cursor-pointer flex-col gap-0.5 rounded px-2 py-1 hover:bg-secondary/60", isDeprecated && "opacity-60")}
      onMouseEnter={(ev) => {
        setHovered(true);
        moveTip(ev.clientX, ev.clientY);
      }}
      onMouseMove={(ev) => moveTip(ev.clientX, ev.clientY)}
      onMouseLeave={() => setHovered(false)}
      onClick={() => onPin?.()}
    >
      <span className="flex w-full items-center justify-between gap-2">
        <span className={cn("flex min-w-0 items-center gap-1 text-foreground", isDeprecated && "line-through decoration-muted-foreground/50")}>
          <span className="truncate">{label}</span>
          {arityBadge}
        </span>
        <span className="flex min-w-0 items-center justify-end gap-1 [flex-shrink:2]">
          {chain.map((item, i) => (
            <Fragment key={i}>
              {i > 0 && (
                <span className="shrink-0 text-[10px] text-muted-foreground/50">→</span>
              )}
              {typeChip(item)}
            </Fragment>
          ))}
        </span>
      </span>
      {deprecatedNote}
      {description && (
        <span className="font-sans text-[11px] leading-snug text-muted-foreground">
          {description}
        </span>
      )}
      {argsDetail}
    </div>
    </>
  );
}

