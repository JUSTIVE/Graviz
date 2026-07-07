import { useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Clock,
  PanelLeftClose,
  PanelLeftOpen,
  Unlink,
  Waypoints,
  type LucideIcon,
} from "lucide-react";
import { SchemaCanvas } from "@/components/graph/SchemaCanvas";
import { TreePanel } from "@/components/tree/TreePanel";
import { KIND_STYLES } from "@/components/graph/node-style";
import { Badge } from "@/components/ui/badge";
import { useSchema, type ExpiredField } from "@/lib/schema-context";
import type { GraphNodeData } from "@/lib/sdl-to-graph";
import { cn } from "@/lib/utils";

type Mode = "reachable" | "orphaned" | "expired";

const SIDEBAR_MIN_W = 260;
const SIDEBAR_MAX_W = 720;
const SIDEBAR_DEFAULT_W = 340;
const SIDEBAR_WIDTH_KEY = "gompassql:sidebarWidth";
const SIDEBAR_COLLAPSED_KEY = "gompassql:sidebarCollapsed";

const clampWidth = (w: number) =>
  Math.min(SIDEBAR_MAX_W, Math.max(SIDEBAR_MIN_W, w));

/** Persisted sidebar width, clamped to a sane range. */
function useSidebarWidth() {
  const [width, setWidth] = useState<number>(() => {
    if (typeof window === "undefined") return SIDEBAR_DEFAULT_W;
    const raw = window.localStorage.getItem(SIDEBAR_WIDTH_KEY);
    const n = raw ? Number.parseInt(raw, 10) : NaN;
    return Number.isFinite(n) ? clampWidth(n) : SIDEBAR_DEFAULT_W;
  });
  useEffect(() => {
    try {
      window.localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
    } catch {
      // ignore
    }
  }, [width]);
  return [width, setWidth] as const;
}

/** Persisted collapsed flag. */
function useSidebarCollapsed() {
  const [collapsed, setCollapsed] = useState<boolean>(() => {
    if (typeof window === "undefined") return false;
    return window.localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === "1";
  });
  useEffect(() => {
    try {
      window.localStorage.setItem(SIDEBAR_COLLAPSED_KEY, collapsed ? "1" : "0");
    } catch {
      // ignore
    }
  }, [collapsed]);
  return [collapsed, setCollapsed] as const;
}

/** Tracks whether the viewport is at the `lg` breakpoint (side-by-side
 *  layout). Resizing / collapsing only applies there — on narrow
 *  screens the panels stack vertically. */
function useIsLg() {
  const [isLg, setIsLg] = useState(
    () => typeof window !== "undefined" && window.matchMedia("(min-width: 1024px)").matches,
  );
  useEffect(() => {
    const mq = window.matchMedia("(min-width: 1024px)");
    const on = () => setIsLg(mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return isLg;
}

export function ViewRoute() {
  const {
    graph,
    focusStack,
    rootType,
    hasSchema,
    visibleNodes,
    visibleEdges,
    orphanedNodes,
    orphanedEdges,
    expiredFields,
    pushFocus,
    popTo,
    setPinnedField,
  } = useSchema();
  const navigate = useNavigate();
  const [mode, setMode] = useState<Mode>("reachable");
  const [orphanFocus, setOrphanFocus] = useState<string | null>(null);
  const [expiredFocus, setExpiredFocus] = useState<string | null>(null);

  const containerRef = useRef<HTMLDivElement>(null);
  const [sidebarWidth, setSidebarWidth] = useSidebarWidth();
  const [collapsed, setCollapsed] = useSidebarCollapsed();
  const isLg = useIsLg();

  // Translate a pointer X into a new sidebar width, measured from the
  // container's left edge and clamped to the allowed range.
  const resizeTo = useCallback(
    (clientX: number) => {
      const left = containerRef.current?.getBoundingClientRect().left ?? 0;
      setSidebarWidth(clampWidth(clientX - left));
    },
    [setSidebarWidth],
  );

  useEffect(() => {
    if (!hasSchema) navigate({ to: "/" });
  }, [hasSchema, navigate]);

  // If every expired field is resolved (e.g. schema swapped), fall back
  // to the reachable view so we never sit on an empty Expired tab.
  useEffect(() => {
    if (mode === "expired" && expiredFields.length === 0) setMode("reachable");
  }, [mode, expiredFields.length]);

  if (!hasSchema) return null;

  const reachableFocusId =
    focusStack.length > 0 ? (focusStack[focusStack.length - 1] ?? null) : rootType;

  const canvasNodes =
    mode === "reachable" ? visibleNodes : mode === "orphaned" ? orphanedNodes : graph.nodes;
  const canvasEdges =
    mode === "reachable" ? visibleEdges : mode === "orphaned" ? orphanedEdges : graph.edges;
  const canvasFocusId =
    mode === "reachable" ? reachableFocusId : mode === "orphaned" ? orphanFocus : expiredFocus;
  const canvasRootId = mode === "reachable" ? rootType : null;

  const onCanvasNavigate =
    mode === "reachable" ? pushFocus : mode === "orphaned" ? setOrphanFocus : setExpiredFocus;
  const onCanvasClearFocus =
    mode === "reachable"
      ? () => popTo(-1)
      : mode === "orphaned"
        ? () => setOrphanFocus(null)
        : () => setExpiredFocus(null);

  // On `lg`, the sidebar width is driven by state (or 0 when collapsed);
  // below `lg` the panels stack and the inline template is dropped so the
  // Tailwind `grid-cols-1` fallback applies.
  const gridStyle =
    isLg && !collapsed ? { gridTemplateColumns: `${sidebarWidth}px 1fr` } : undefined;

  return (
    <div
      ref={containerRef}
      className={cn(
        "grid min-h-0 flex-1",
        collapsed
          ? "grid-cols-1"
          : "grid-cols-1 lg:grid-cols-[minmax(300px,380px)_1fr]",
      )}
      style={gridStyle}
    >
      {!collapsed && (
      <aside className="relative flex min-h-0 flex-col border-b border-border bg-card/30 lg:border-b-0 lg:border-r">
        {/* Mode tab switcher. Inactive tabs collapse to their icon; the
            active tab expands to fill the row with icon + label. */}
        <div className="flex shrink-0 items-stretch border-b border-border">
          <ModeTab
            active={mode === "reachable"}
            onClick={() => setMode("reachable")}
            label="Reachable"
            icon={Waypoints}
          />
          <ModeTab
            active={mode === "orphaned"}
            onClick={() => setMode("orphaned")}
            label="Orphaned"
            icon={Unlink}
            count={orphanedNodes.length}
            warn={orphanedNodes.length > 0}
            disabled={orphanedNodes.length === 0}
          />
          {/* Expired tab is hidden entirely when nothing is overdue. */}
          {expiredFields.length > 0 && (
            <ModeTab
              active={mode === "expired"}
              onClick={() => setMode("expired")}
              label="Expired"
              icon={Clock}
              count={expiredFields.length}
              warn
              tone="red"
            />
          )}
          <button
            type="button"
            onClick={() => setCollapsed(true)}
            title="Collapse sidebar"
            aria-label="Collapse sidebar"
            className="flex shrink-0 items-center justify-center border-l border-border px-2.5 text-muted-foreground transition-colors hover:bg-secondary/60 hover:text-foreground"
          >
            <PanelLeftClose className="h-4 w-4" />
          </button>
        </div>

        {mode === "reachable" ? (
          <TreePanel />
        ) : mode === "orphaned" ? (
          <OrphanPanel
            nodes={orphanedNodes}
            focusId={orphanFocus}
            onFocus={setOrphanFocus}
          />
        ) : (
          <ExpiredPanel
            fields={expiredFields}
            focusId={expiredFocus}
            onSelect={(f) => {
              setExpiredFocus(f.typeId);
              setPinnedField({
                typeId: f.typeId,
                fieldName: f.fieldName,
                fieldIndex: f.fieldIndex,
              });
            }}
          />
        )}

        {/* Drag handle — only meaningful in the side-by-side layout. */}
        {isLg && <ResizeHandle onResize={resizeTo} onReset={() => setSidebarWidth(SIDEBAR_DEFAULT_W)} />}
      </aside>
      )}

      <section className="relative min-h-[500px] flex-1">
        {collapsed && (
          <button
            type="button"
            onClick={() => setCollapsed(false)}
            title="Show sidebar"
            aria-label="Show sidebar"
            className="absolute left-2 top-2 z-20 flex items-center justify-center rounded-md border border-border bg-card/80 p-1.5 text-muted-foreground shadow-sm backdrop-blur transition-colors hover:bg-secondary hover:text-foreground"
          >
            <PanelLeftOpen className="h-4 w-4" />
          </button>
        )}
        <SchemaCanvas
          nodes={canvasNodes}
          edges={canvasEdges}
          focusId={canvasFocusId}
          rootId={canvasRootId}
          onNavigate={onCanvasNavigate}
          onClearFocus={onCanvasClearFocus}
        />
      </section>
    </div>
  );
}

function ModeTab({
  active,
  onClick,
  label,
  icon: Icon,
  count,
  warn,
  disabled,
  tone = "amber",
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  icon: LucideIcon;
  count?: number;
  warn?: boolean;
  disabled?: boolean;
  tone?: "amber" | "red";
}) {
  const red = tone === "red";
  // The warn state tints the tab's own icon (amber/red) so an inactive,
  // icon-only tab still signals that it has something worth a look.
  const warnIconColor = warn
    ? red
      ? active ? "text-red-500" : "text-red-500/70"
      : active ? "text-amber-500" : "text-amber-500/70"
    : undefined;
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={label}
      aria-label={label}
      className={cn(
        "flex items-center justify-center gap-1.5 py-2 text-xs font-medium transition-colors",
        // Active tab fills the remaining width; inactive tabs shrink to
        // an icon-only square.
        active
          ? "flex-1 border-b-2 border-primary text-foreground"
          : "px-3 text-muted-foreground hover:text-foreground",
        disabled && "cursor-default opacity-40",
      )}
    >
      <Icon className={cn("h-4 w-4 shrink-0", warnIconColor)} />
      {active && (
        <>
          {label}
          {count != null && count > 0 && (
            <span
              className={cn(
                "rounded-full px-1.5 py-px text-[10px] leading-none",
                red
                  ? "bg-red-500/15 text-red-600 dark:text-red-400"
                  : "bg-amber-500/15 text-amber-600 dark:text-amber-400",
              )}
            >
              {count}
            </span>
          )}
        </>
      )}
    </button>
  );
}

/**
 * A thin drag strip sitting on the sidebar's right edge. While dragging
 * it listens on the window (so the pointer can leave the strip) and
 * suppresses text selection. Double-click resets to the default width.
 */
function ResizeHandle({
  onResize,
  onReset,
}: {
  onResize: (clientX: number) => void;
  onReset: () => void;
}) {
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    if (!dragging) return;
    const move = (e: PointerEvent) => onResize(e.clientX);
    const up = () => setDragging(false);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    const prevSelect = document.body.style.userSelect;
    const prevCursor = document.body.style.cursor;
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    return () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.style.userSelect = prevSelect;
      document.body.style.cursor = prevCursor;
    };
  }, [dragging, onResize]);

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      onPointerDown={(e) => {
        e.preventDefault();
        setDragging(true);
      }}
      onDoubleClick={onReset}
      className={cn(
        "absolute right-0 top-0 z-10 hidden h-full w-1.5 translate-x-1/2 cursor-col-resize lg:block",
        "transition-colors",
        dragging ? "bg-primary/40" : "hover:bg-primary/25",
      )}
    />
  );
}

const KIND_ORDER: GraphNodeData["kind"][] = [
  "Object", "Interface", "Union", "Enum", "Input", "Scalar",
];

function OrphanPanel({
  nodes,
  focusId,
  onFocus,
}: {
  nodes: GraphNodeData[];
  focusId: string | null;
  onFocus: (id: string) => void;
}) {
  const grouped = useMemo(() => {
    const map = new Map<GraphNodeData["kind"], GraphNodeData[]>();
    for (const n of nodes) {
      if (!map.has(n.kind)) map.set(n.kind, []);
      map.get(n.kind)!.push(n);
    }
    for (const list of map.values()) list.sort((a, b) => a.name.localeCompare(b.name));
    return KIND_ORDER.flatMap((k) => {
      const list = map.get(k);
      return list ? [{ kind: k, nodes: list }] : [];
    });
  }, [nodes]);

  if (nodes.length === 0) {
    return (
      <div className="flex flex-1 items-center justify-center p-6 text-center text-xs text-muted-foreground">
        No orphaned types found.
      </div>
    );
  }

  return (
    <div className="min-h-0 flex-1 overflow-auto [scrollbar-width:none] [&_::-webkit-scrollbar]:w-0">
      <div className="px-3 py-2 text-[10px] text-muted-foreground">
        {nodes.length} type{nodes.length !== 1 ? "s" : ""} unreachable from root
      </div>
      {grouped.map(({ kind, nodes: list }) => {
        const style = KIND_STYLES[kind];
        return (
          <div key={kind}>
            <div className="sticky top-0 bg-card/90 px-3 py-1 text-[10px] uppercase tracking-wider text-muted-foreground backdrop-blur">
              {kind} <span className="opacity-60">({list.length})</span>
            </div>
            <ul>
              {list.map((n) => (
                <li key={n.id}>
                  <button
                    type="button"
                    onClick={() => onFocus(n.id)}
                    className={cn(
                      "flex w-full items-center gap-2 px-3 py-1.5 text-left font-mono text-xs transition-colors",
                      focusId === n.id
                        ? "bg-primary/10 text-primary"
                        : "hover:bg-secondary/60",
                    )}
                  >
                    <Badge className={cn("shrink-0 px-1.5 py-0 text-[9px] leading-4", style.badge)}>
                      {style.label}
                    </Badge>
                    <span className="truncate">{n.name}</span>
                    {n.fields && (
                      <span className="ml-auto shrink-0 text-[10px] text-muted-foreground/60">
                        {n.fields.length}f
                      </span>
                    )}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        );
      })}
    </div>
  );
}

/** Whole numbers of days between `until` (end of that day) and now. */
function daysOverdue(until: string): number {
  const end = Date.parse(`${until}T23:59:59.999`);
  if (!Number.isFinite(end)) return 0;
  return Math.floor((Date.now() - end) / 86_400_000);
}

function ExpiredPanel({
  fields,
  focusId,
  onSelect,
}: {
  fields: ExpiredField[];
  focusId: string | null;
  onSelect: (f: ExpiredField) => void;
}) {
  // Group by owning type, preserving the most-overdue-first order the
  // context already sorted the flat list into.
  const grouped = useMemo(() => {
    const map = new Map<string, ExpiredField[]>();
    for (const f of fields) {
      if (!map.has(f.typeId)) map.set(f.typeId, []);
      map.get(f.typeId)!.push(f);
    }
    return [...map.entries()].map(([typeId, list]) => ({ typeId, list }));
  }, [fields]);

  if (fields.length === 0) return null;

  return (
    <div className="min-h-0 flex-1 overflow-auto [scrollbar-width:none] [&_::-webkit-scrollbar]:w-0">
      <div className="px-3 py-2 text-[10px] text-muted-foreground">
        {fields.length} field{fields.length !== 1 ? "s" : ""} past their{" "}
        <span className="font-mono text-red-500">until</span> date
      </div>
      {grouped.map(({ typeId, list }) => {
        const style = KIND_STYLES[list[0]!.typeKind];
        return (
          <div key={typeId}>
            <div className="sticky top-0 flex items-center gap-1.5 bg-card/90 px-3 py-1 text-[10px] backdrop-blur">
              <Badge className={cn("shrink-0 px-1.5 py-0 text-[9px] leading-4", style.badge)}>
                {style.label}
              </Badge>
              <span className="truncate font-mono text-muted-foreground">{list[0]!.typeName}</span>
              <span className="opacity-60">({list.length})</span>
            </div>
            <ul>
              {list.map((f) => {
                const overdue = daysOverdue(f.until);
                return (
                  <li key={`${f.typeId}.${f.fieldName}.${f.fieldIndex}`}>
                    <button
                      type="button"
                      onClick={() => onSelect(f)}
                      className={cn(
                        "flex w-full flex-col gap-0.5 px-3 py-1.5 text-left transition-colors",
                        focusId === f.typeId ? "bg-red-500/10" : "hover:bg-secondary/60",
                      )}
                    >
                      <span className="flex w-full items-center gap-2 font-mono text-xs">
                        <span className="truncate text-red-600 line-through decoration-red-500/50 dark:text-red-400">
                          {f.fieldName}
                        </span>
                        <span className="ml-auto shrink-0 font-mono text-[10px] text-red-500">
                          {f.until}
                        </span>
                      </span>
                      <span className="text-[10px] text-muted-foreground">
                        {overdue > 0 ? `${overdue.toLocaleString()}d overdue` : "overdue"}
                        {f.deprecationReason ? ` · ${stripUntil(f.deprecationReason)}` : ""}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        );
      })}
    </div>
  );
}

/** Drops the leading `[until …]` marker from a reason for display —
 *  the date is already shown as its own chip. */
function stripUntil(reason: string): string {
  return reason.replace(/\[\s*until\s+\d{4}-\d{2}-\d{2}\s*\]\s*/i, "").trim();
}
