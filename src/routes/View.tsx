import { useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ChevronDown,
  ChevronUp,
  Clock,
  Highlighter,
  Layers,
  PanelLeftClose,
  PanelLeftOpen,
  Unlink,
  Waypoints,
  type LucideIcon,
} from "lucide-react";
import { SchemaCanvas } from "@/components/graph/SchemaCanvas";
import { SdlEditor } from "@/components/SdlEditor";
import { TreePanel } from "@/components/tree/TreePanel";
import { KIND_STYLES } from "@/components/graph/node-style";
import { Badge } from "@/components/ui/badge";
import type { OverlayDiff } from "@/lib/overlay";
import { useSchema, type UntilField } from "@/lib/schema-context";
import type { GraphNodeData } from "@/lib/sdl-to-graph";
import { cn } from "@/lib/utils";

type Mode = "reachable" | "orphaned" | "until";

const OVERLAY_PLACEHOLDER = `# Sketch on top of the loaded schema — nothing here modifies it.
# A type the schema already declares is augmented, so you can paste
# one in and just add to it. Restating a field overrides it, and a
# "-name" line takes it away.

type Query {
  report(id: ID!): Report
}

type Report {
  id: ID!
  title: String!
}

type User {
  name: String!      # override: was nullable
  -legacyName        # drop a field
  displayName: String
}`;

const OVERLAY_MIN_H = 160;
const OVERLAY_MAX_H = 720;
const OVERLAY_DEFAULT_H = 280;
const OVERLAY_HEIGHT_KEY = "gompassql:overlayHeight";

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

/** Overlay dock height, clamped so the canvas always keeps a usable
 *  slice of the viewport even on a short window. */
const clampHeight = (h: number) => {
  const ceiling =
    typeof window === "undefined"
      ? OVERLAY_MAX_H
      : Math.max(OVERLAY_MIN_H, Math.min(OVERLAY_MAX_H, window.innerHeight * 0.7));
  return Math.min(ceiling, Math.max(OVERLAY_MIN_H, h));
};

/** Persisted overlay dock height. */
function useOverlayHeight() {
  const [height, setHeight] = useState<number>(() => {
    if (typeof window === "undefined") return OVERLAY_DEFAULT_H;
    const raw = window.localStorage.getItem(OVERLAY_HEIGHT_KEY);
    const n = raw ? Number.parseInt(raw, 10) : NaN;
    return Number.isFinite(n) ? clampHeight(n) : OVERLAY_DEFAULT_H;
  });
  useEffect(() => {
    try {
      window.localStorage.setItem(OVERLAY_HEIGHT_KEY, String(height));
    } catch {
      // ignore
    }
  }, [height]);
  return [height, setHeight] as const;
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
    upcomingFields,
    deprecatedFields,
    pushFocus,
    popTo,
    pinnedField,
    setPinnedField,
    overlaySdl,
    applyOverlay,
    clearOverlay,
    overlayError,
    overlayDiff,
    overlayWarnings,
  } = useSchema();
  const navigate = useNavigate();
  const [mode, setMode] = useState<Mode>("reachable");
  const [orphanFocus, setOrphanFocus] = useState<string | null>(null);
  const [untilFocus, setUntilFocus] = useState<string | null>(null);
  // Highlight mode lives here, not in the dock: the canvas needs it too,
  // and it has to survive collapsing the dock.
  const [highlightOverlay, setHighlightOverlay] = useState(false);

  // Only additions and overrides produce overlay-marked nodes, so an
  // overlay that merely removes things has nothing to highlight.
  const hasOverlayTypes =
    overlayDiff.addedTypes.length +
      overlayDiff.addedFields.length +
      overlayDiff.changedFields.length >
    0;

  // The "Deprecated" tab exists whenever the schema has any deprecated
  // field — expired, upcoming, or undated.
  const deprecatedCount =
    expiredFields.length + upcomingFields.length + deprecatedFields.length;
  const hasDeprecated = deprecatedCount > 0;

  const containerRef = useRef<HTMLDivElement>(null);
  const [sidebarWidth, setSidebarWidth] = useSidebarWidth();
  const [collapsed, setCollapsed] = useSidebarCollapsed();
  const isLg = useIsLg();
  // While the user drags the resize handle we suppress the width
  // transition so the sidebar tracks the pointer 1:1 instead of easing
  // behind it. The transition is only wanted for the collapse/expand
  // toggle.
  const [isResizing, setIsResizing] = useState(false);

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

  // If the schema swaps to one with no dated fields at all, fall back to
  // the reachable view so we never sit on an empty Until tab.
  useEffect(() => {
    if (mode === "until" && !hasDeprecated) setMode("reachable");
  }, [mode, hasDeprecated]);

  // Focusing an overlay type and then clearing (or breaking) the overlay
  // leaves a focus id pointing at a node that no longer exists — the
  // canvas would dim every node against a target it can't find. Drop the
  // focus as soon as its node goes away. (`focusStack` and `pinnedField`
  // get the same treatment inside the schema context.)
  useEffect(() => {
    const gone = (id: string | null) => id != null && !graph.nodes.some((n) => n.id === id);
    if (gone(orphanFocus)) setOrphanFocus(null);
    if (gone(untilFocus)) setUntilFocus(null);
  }, [orphanFocus, untilFocus, graph]);

  // Clearing (or breaking) the overlay leaves highlight mode dimming the
  // whole canvas against an empty set — drop it as soon as there's
  // nothing left to highlight.
  useEffect(() => {
    if (!hasOverlayTypes && highlightOverlay) setHighlightOverlay(false);
  }, [hasOverlayTypes, highlightOverlay]);

  // Mobile is single-pane: the tree fills the screen and the canvas is
  // hidden behind it. When the user selects something in the tree
  // (focus push or field pin), collapse the sidebar so the canvas —
  // now framed on that selection — comes to the front. Guarded on a
  // real change so it never fires on mount or on unrelated re-renders.
  const prevSelRef = useRef<string>("");
  useEffect(() => {
    const sel =
      focusStack.join(">") +
      "|" +
      (pinnedField ? `${pinnedField.typeId}.${pinnedField.fieldName}` : "");
    if (prevSelRef.current && sel !== prevSelRef.current && !isLg && !collapsed) {
      setCollapsed(true);
    }
    prevSelRef.current = sel;
  }, [focusStack, pinnedField, isLg, collapsed, setCollapsed]);

  // Cmd/Ctrl+K opens the tree search even when the sidebar is collapsed:
  // expand it first, then bump a key the TreePanel watches to focus its
  // input. Focusing here (rather than inside TreePanel) guarantees the
  // panel is un-hidden before the focus lands — on mobile a collapsed
  // sidebar is display:none and can't receive focus until it re-renders.
  const [searchFocusKey, setSearchFocusKey] = useState(0);
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setCollapsed(false);
        setSearchFocusKey((k) => k + 1);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [setCollapsed]);

  if (!hasSchema) return null;

  const reachableFocusId =
    focusStack.length > 0 ? (focusStack[focusStack.length - 1] ?? null) : rootType;

  const canvasNodes =
    mode === "reachable" ? visibleNodes : mode === "orphaned" ? orphanedNodes : graph.nodes;
  const canvasEdges =
    mode === "reachable" ? visibleEdges : mode === "orphaned" ? orphanedEdges : graph.edges;
  const canvasFocusId =
    mode === "reachable" ? reachableFocusId : mode === "orphaned" ? orphanFocus : untilFocus;
  const canvasRootId = mode === "reachable" ? rootType : null;

  const onCanvasNavigate =
    mode === "reachable" ? pushFocus : mode === "orphaned" ? setOrphanFocus : setUntilFocus;
  const onCanvasClearFocus =
    mode === "reachable"
      ? () => popTo(-1)
      : mode === "orphaned"
        ? () => setOrphanFocus(null)
        : () => setUntilFocus(null);

  // On `lg` the sidebar column width is state-driven and animates to 0
  // on collapse (the aside stays mounted and its content clips). Below
  // `lg` the panels stack, so we drop the inline template and toggle the
  // aside with `hidden` instead.
  const gridStyle = isLg
    ? { gridTemplateColumns: collapsed ? "0px 1fr" : `${sidebarWidth}px 1fr` }
    : undefined;

  return (
    <div
      ref={containerRef}
      className={cn(
        // Mobile is single-pane: one full-height row holds whichever of
        // aside/section is visible (the other is `hidden`). On `lg` the
        // two go side by side and rows reset to auto.
        "grid min-h-0 flex-1 grid-cols-1 grid-rows-[1fr] lg:grid-cols-[minmax(300px,380px)_1fr] lg:grid-rows-none",
        // Animate the collapse/expand, but not while dragging the handle.
        !isResizing && "transition-[grid-template-columns] duration-300 ease-out",
      )}
      style={gridStyle}
    >
      <aside
        className={cn(
          "relative flex min-h-0 min-w-0 flex-col overflow-hidden border-b border-border bg-card/30 lg:border-b-0 lg:border-r",
          // Drop the divider line when fully collapsed on `lg`, and hide
          // the row entirely when stacked (non-`lg`).
          collapsed && "lg:border-r-0",
          collapsed && !isLg && "hidden",
        )}
      >
        {/* On `lg` the content keeps a fixed pixel width while the grid
            column animates, so collapsing clips (masks) it instead of
            reflowing text at every frame. */}
        <div
          className="flex min-h-0 flex-1 flex-col"
          style={isLg ? { width: sidebarWidth } : undefined}
        >
        {/* Mode tab switcher. Inactive tabs collapse to their icon; the
            active tab expands to fill the row with icon + label. */}
        <div className="flex shrink-0 items-stretch border-b border-border">
          <ModeTab
            active={mode === "reachable"}
            onClick={() => setMode("reachable")}
            label="Reachable"
            icon={Waypoints}
            tone="sky"
          />
          <ModeTab
            active={mode === "orphaned"}
            onClick={() => setMode("orphaned")}
            label="Orphaned"
            icon={Unlink}
            count={orphanedNodes.length}
            tone="amber"
            disabled={orphanedNodes.length === 0}
          />
          {/* Deprecated tab is hidden entirely when the schema has no
              deprecated fields. Red when something is already overdue,
              else amber. */}
          {hasDeprecated && (
            <ModeTab
              active={mode === "until"}
              onClick={() => setMode("until")}
              label="Deprecated"
              icon={Clock}
              count={deprecatedCount}
              tone={expiredFields.length > 0 ? "red" : "amber"}
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
          <TreePanel searchFocusKey={searchFocusKey} />
        ) : mode === "orphaned" ? (
          <OrphanPanel
            nodes={orphanedNodes}
            focusId={orphanFocus}
            onFocus={setOrphanFocus}
          />
        ) : (
          <UntilPanel
            expired={expiredFields}
            upcoming={upcomingFields}
            deprecated={deprecatedFields}
            focusId={untilFocus}
            onSelect={(f) => {
              setUntilFocus(f.typeId);
              setPinnedField({
                typeId: f.typeId,
                fieldName: f.fieldName,
                fieldIndex: f.fieldIndex,
              });
            }}
          />
        )}
        </div>

        {/* Drag handle — only in the expanded side-by-side layout. */}
        {isLg && !collapsed && (
          <ResizeHandle
            onResize={resizeTo}
            onReset={() => setSidebarWidth(SIDEBAR_DEFAULT_W)}
            onDraggingChange={setIsResizing}
          />
        )}
      </aside>

      {/* The canvas column stacks the graph over the overlay dock, so the
          dock spans the graph only — the tree keeps its full height, the
          way an editor's bottom panel leaves the file tree alone. */}
      <section
        className={cn(
          "flex min-w-0 flex-1 flex-col overflow-hidden lg:min-h-[500px]",
          // On mobile the canvas hides behind the full-screen tree while
          // the sidebar is open; it returns when the sidebar collapses.
          !collapsed && !isLg && "hidden",
        )}
      >
        <div className="relative min-h-0 flex-1">
          {/* Desktop: floating expand affordance at the canvas top-left
              (the canvas pushes its own controls down to clear it). On
              mobile the expand button lives inside the merged view-controls
              header instead (see SchemaCanvas onExpandSidebar), so it never
              overlaps the top dock. */}
          {collapsed && isLg && (
            <button
              type="button"
              onClick={() => setCollapsed(false)}
              title="Show sidebar"
              aria-label="Show sidebar"
              className="absolute left-4 top-4 z-30 flex items-center justify-center rounded-lg border border-border bg-popover/95 p-1.5 text-muted-foreground shadow-lg backdrop-blur transition-colors hover:bg-secondary hover:text-foreground"
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
            leftControlsInset={collapsed}
            onExpandSidebar={() => setCollapsed(false)}
            highlightOverlay={highlightOverlay}
          />
        </div>

        <OverlayDock
          applied={overlaySdl}
          onApply={applyOverlay}
          onClear={clearOverlay}
          error={overlayError}
          diff={overlayDiff}
          warnings={overlayWarnings}
          highlight={highlightOverlay}
          canHighlight={hasOverlayTypes}
          onToggleHighlight={() => setHighlightOverlay((v) => !v)}
          // Chips focus through whichever mode the canvas is in, so
          // clicking one lands the same way a canvas click would.
          onFocusType={onCanvasNavigate}
        />
      </section>
    </div>
  );
}

type Tone = "sky" | "amber" | "red" | "emerald";

// Each tab carries one accent color, shared by its icon and — when
// active — its underline, label text, and count badge, so the active
// highlight always matches the tab's own icon color.
const TAB_TONE: Record<Tone, { icon: string; text: string; border: string; badge: string }> = {
  sky: {
    icon: "text-sky-500",
    text: "text-sky-600 dark:text-sky-400",
    border: "border-sky-500",
    badge: "bg-sky-500/15 text-sky-600 dark:text-sky-400",
  },
  amber: {
    icon: "text-amber-500",
    text: "text-amber-600 dark:text-amber-400",
    border: "border-amber-500",
    badge: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
  },
  red: {
    icon: "text-red-500",
    text: "text-red-600 dark:text-red-400",
    border: "border-red-500",
    badge: "bg-red-500/15 text-red-600 dark:text-red-400",
  },
  emerald: {
    icon: "text-emerald-500",
    text: "text-emerald-600 dark:text-emerald-400",
    border: "border-emerald-500",
    badge: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  },
};

function ModeTab({
  active,
  onClick,
  label,
  icon: Icon,
  count,
  disabled,
  tone = "amber",
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  icon: LucideIcon;
  count?: number;
  disabled?: boolean;
  tone?: Tone;
}) {
  const t = TAB_TONE[tone];
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={label}
      aria-label={label}
      // Active tab grows to fill the row; inactive tabs shrink to an
      // icon-only square. Animating flex-grow (plus the label's
      // max-width/opacity) makes the swap slide rather than snap.
      style={{ flexGrow: active ? 1 : 0 }}
      className={cn(
        "flex items-center justify-center border-b-2 px-3 py-2 text-xs font-medium",
        "transition-[flex-grow,color,border-color,background-color] duration-300 ease-out",
        // Active: underline + label take the tab's accent color.
        active
          ? cn(t.border, t.text)
          : "border-transparent text-muted-foreground hover:text-foreground",
        disabled && "cursor-default opacity-40",
      )}
    >
      {/* Icon always wears the tab's accent color (dimmed while inactive). */}
      <Icon className={cn("h-4 w-4 shrink-0", t.icon, !active && "opacity-70")} />
      <span
        className={cn(
          "flex items-center overflow-hidden whitespace-nowrap transition-all duration-300 ease-out",
          active ? "ml-1.5 max-w-[200px] opacity-100" : "ml-0 max-w-0 opacity-0",
        )}
      >
        {label}
        {count != null && count > 0 && (
          <span className={cn("ml-1.5 rounded-full px-1.5 py-px text-[10px] leading-none", t.badge)}>
            {count}
          </span>
        )}
      </span>
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
  onDraggingChange,
}: {
  onResize: (clientX: number) => void;
  onReset: () => void;
  onDraggingChange?: (dragging: boolean) => void;
}) {
  const [dragging, setDragging] = useState(false);

  // Let the parent suppress the width transition while dragging.
  useEffect(() => {
    onDraggingChange?.(dragging);
  }, [dragging, onDraggingChange]);

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

/**
 * A thin drag strip along the overlay dock's top edge. Mirror of
 * {@link ResizeHandle}, one axis over. Double-click resets the height.
 */
function VerticalResizeHandle({
  onResize,
  onReset,
}: {
  onResize: (clientY: number) => void;
  onReset: () => void;
}) {
  const [dragging, setDragging] = useState(false);

  useEffect(() => {
    if (!dragging) return;
    const move = (e: PointerEvent) => onResize(e.clientY);
    const up = () => setDragging(false);
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    const prevSelect = document.body.style.userSelect;
    const prevCursor = document.body.style.cursor;
    document.body.style.userSelect = "none";
    document.body.style.cursor = "row-resize";
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
      aria-orientation="horizontal"
      onPointerDown={(e) => {
        e.preventDefault();
        setDragging(true);
      }}
      onDoubleClick={onReset}
      className={cn(
        "absolute left-0 top-0 z-10 h-1.5 w-full -translate-y-1/2 cursor-row-resize transition-colors",
        dragging ? "bg-primary/40" : "hover:bg-primary/25",
      )}
    />
  );
}

/** `+3 ~1 −1` pills summarizing an applied overlay. */
function OverlayCounts({
  added,
  changed,
  removed,
}: {
  added: number;
  changed: number;
  removed: number;
}) {
  if (added === 0 && changed === 0 && removed === 0) {
    return <span className="text-[10px] text-muted-foreground">no change</span>;
  }
  return (
    <span className="flex items-center gap-1">
      {added > 0 && (
        <span className="rounded-full bg-emerald-500/15 px-1.5 py-px text-[10px] leading-none text-emerald-600 dark:text-emerald-400">
          +{added}
        </span>
      )}
      {changed > 0 && (
        <span className="rounded-full bg-sky-500/15 px-1.5 py-px text-[10px] leading-none text-sky-600 dark:text-sky-400">
          ~{changed}
        </span>
      )}
      {removed > 0 && (
        <span className="rounded-full bg-red-500/15 px-1.5 py-px text-[10px] leading-none text-red-600 dark:text-red-400">
          −{removed}
        </span>
      )}
    </span>
  );
}

/**
 * The overlay dock: a GraphQL editor pinned under the canvas whose
 * contents are laid over the loaded schema on Apply. The schema itself is
 * never modified — Clear puts the graph back exactly as it was.
 *
 * Collapsed, it's a one-line strip reporting whatever is applied; open, it
 * splits the canvas column and can be dragged taller. It never unmounts,
 * so the draft survives collapsing — and keeping the buffer here rather
 * than in the schema context means a keystroke doesn't re-render the
 * canvas and the tree.
 *
 * Three things the editor understands (see `lib/overlay.ts`): a bare type
 * the schema already declares is augmented rather than redeclared, a
 * `-name` line removes that field, and everything the overlay adds is
 * painted emerald on the canvas.
 */
function OverlayDock({
  applied,
  onApply,
  onClear,
  error,
  diff,
  warnings,
  highlight,
  canHighlight,
  onToggleHighlight,
  onFocusType,
}: {
  applied: string;
  onApply: (sdl: string) => void;
  onClear: () => void;
  error: string | null;
  diff: OverlayDiff;
  warnings: string[];
  highlight: boolean;
  canHighlight: boolean;
  onToggleHighlight: () => void;
  onFocusType: (typeName: string) => void;
}) {
  const [open, setOpen] = useState(false);
  // The editor always starts from whatever is applied, so remounting the
  // route never leaves an applied overlay with an empty buffer to edit it
  // from. Clearing keeps the text — that's the point of Re-apply.
  const [draft, setDraft] = useState(applied);
  const [height, setHeight] = useOverlayHeight();
  const dockRef = useRef<HTMLDivElement>(null);

  // Same guarantee for an apply this dock didn't originate. Keyed on the
  // applied value changing, never on the draft, so emptying the editor by
  // hand doesn't get undone under the user's cursor.
  const syncedRef = useRef(applied);
  useEffect(() => {
    if (applied === syncedRef.current) return;
    syncedRef.current = applied;
    if (applied.trim() && !draft.trim()) setDraft(applied);
  }, [applied, draft]);

  const isApplied = applied.trim().length > 0;
  const dirty = draft.trim() !== applied.trim();
  const added = diff.addedTypes.length + diff.addedFields.length;
  const changed = diff.changedFields.length;
  const removed = diff.removedTypes.length + diff.removedFields.length;

  // Pointer Y → height, measured up from the dock's bottom edge — which
  // stays put, since the dock is flush with the bottom of the column.
  const resizeTo = useCallback(
    (clientY: number) => {
      const bottom = dockRef.current?.getBoundingClientRect().bottom ?? 0;
      setHeight(clampHeight(bottom - clientY));
    },
    [setHeight],
  );

  if (!open) {
    return (
      <button
        type="button"
        onClick={() => setOpen(true)}
        title="Open the overlay editor"
        className="flex shrink-0 items-center gap-2 border-t border-border bg-card/40 px-3 py-1.5 text-left transition-colors hover:bg-secondary/50"
      >
        <Layers className="h-3.5 w-3.5 shrink-0 text-emerald-500" />
        <span className="shrink-0 text-xs font-medium">Overlay</span>
        {error ? (
          <span className="truncate text-[10px] text-red-600 dark:text-red-400">
            not applied — {error}
          </span>
        ) : isApplied ? (
          <OverlayCounts added={added} changed={changed} removed={removed} />
        ) : (
          <span className="truncate text-[10px] text-muted-foreground">
            Sketch SDL on top of this schema
          </span>
        )}
        <ChevronUp className="ml-auto h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      </button>
    );
  }

  const changeLabel =
    [
      added > 0 ? `${added} addition${added !== 1 ? "s" : ""}` : null,
      changed > 0 ? `${changed} override${changed !== 1 ? "s" : ""}` : null,
      removed > 0 ? `${removed} removal${removed !== 1 ? "s" : ""}` : null,
    ]
      .filter(Boolean)
      .join(", ") || "no change";

  return (
    <div
      ref={dockRef}
      style={{ height }}
      className="relative flex shrink-0 flex-col border-t border-border bg-card/40"
      // Cmd/Ctrl+Enter applies from anywhere in the dock, including
      // inside CodeMirror — the editor doesn't bind that chord itself.
      onKeyDown={(e) => {
        if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
          e.preventDefault();
          onApply(draft);
        }
      }}
    >
      <VerticalResizeHandle onResize={resizeTo} onReset={() => setHeight(OVERLAY_DEFAULT_H)} />

      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-1.5">
        <Layers className="h-3.5 w-3.5 shrink-0 text-emerald-500" />
        <span className="shrink-0 text-xs font-medium">Overlay</span>
        {isApplied && !error && <OverlayCounts added={added} changed={changed} removed={removed} />}

        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          {/* Isolate what the overlay contributed: everything it didn't
              touch fades out on the canvas. The aura stays on either
              way, so this is emphasis, not the only cue. */}
          <button
            type="button"
            onClick={onToggleHighlight}
            disabled={!canHighlight}
            title={
              canHighlight
                ? "Dim everything the overlay didn't touch"
                : "Nothing overlaid to highlight yet"
            }
            aria-pressed={highlight}
            className={cn(
              "flex items-center gap-1 rounded-md border px-2 py-1 text-xs font-medium transition-colors",
              !canHighlight
                ? "cursor-default border-border opacity-40"
                : highlight
                  ? "border-emerald-500 bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
                  : "border-border text-muted-foreground hover:bg-secondary hover:text-foreground",
            )}
          >
            <Highlighter className="h-3.5 w-3.5" />
            <span className="hidden sm:inline">Highlight</span>
          </button>
          <span className="hidden text-[10px] text-muted-foreground sm:inline">
            {dirty && isApplied ? "Unapplied edits · " : ""}⌘↵
          </span>
          <button
            type="button"
            onClick={() => onApply(draft)}
            disabled={!dirty && isApplied}
            className={cn(
              "rounded-md px-2.5 py-1 text-xs font-medium transition-colors",
              !dirty && isApplied
                ? "cursor-default bg-secondary text-muted-foreground"
                : "bg-emerald-500 text-white hover:bg-emerald-600",
            )}
          >
            {isApplied && dirty ? "Re-apply" : "Apply"}
          </button>
          <button
            type="button"
            onClick={onClear}
            disabled={!isApplied}
            className={cn(
              "rounded-md border border-border px-2.5 py-1 text-xs font-medium transition-colors",
              isApplied
                ? "text-muted-foreground hover:bg-secondary hover:text-foreground"
                : "cursor-default opacity-40",
            )}
          >
            Clear
          </button>
          <button
            type="button"
            onClick={() => setOpen(false)}
            title="Collapse the overlay editor"
            aria-label="Collapse the overlay editor"
            className="rounded-md p-1 text-muted-foreground transition-colors hover:bg-secondary hover:text-foreground"
          >
            <ChevronDown className="h-4 w-4" />
          </button>
        </div>
      </div>

      <div className="relative min-h-0 flex-1">
        <SdlEditor value={draft} onChange={setDraft} placeholder={OVERLAY_PLACEHOLDER} />
      </div>

      {/* Status strips. A parse failure wins over the applied summary,
          since a failing overlay contributes nothing; warnings stack
          underneath either, because they are about individual lines
          rather than the document as a whole. */}
      {(error || isApplied || warnings.length > 0) && (
        <div className="max-h-[45%] shrink-0 overflow-auto border-t border-border">
          {error ? (
            <div className="bg-red-500/10 px-3 py-2">
              <div className="text-[10px] font-semibold uppercase tracking-wider text-red-600 dark:text-red-400">
                Overlay not applied
              </div>
              <div className="mt-0.5 break-words font-mono text-[10px] leading-snug text-red-600/90 dark:text-red-400/90">
                {error}
              </div>
              <div className="mt-1 text-[10px] text-muted-foreground">
                The graph is still showing the schema without the overlay.
              </div>
            </div>
          ) : isApplied ? (
            <div className="bg-emerald-500/10 px-3 py-2">
              <div className="text-[10px] font-semibold uppercase tracking-wider text-emerald-600 dark:text-emerald-400">
                Overlay applied
                <span className="ml-1 opacity-70">({changeLabel})</span>
              </div>
              {added + changed + removed === 0 && (
                <div className="mt-0.5 text-[10px] text-muted-foreground">
                  Nothing changed — every type and field already matches the schema.
                </div>
              )}
              {diff.addedTypes.length > 0 && (
                <div className="mt-1 flex flex-wrap gap-1">
                  {diff.addedTypes.map((t) => (
                    <button
                      key={t}
                      type="button"
                      onClick={() => onFocusType(t)}
                      title={`Focus ${t}`}
                      className="rounded bg-emerald-500/20 px-1.5 py-px font-mono text-[10px] text-emerald-700 transition-colors hover:bg-emerald-500/35 dark:text-emerald-300"
                    >
                      {t}
                    </button>
                  ))}
                </div>
              )}
              {diff.addedFields.length > 0 && (
                <div className="mt-1 flex flex-wrap gap-x-2 gap-y-0.5 font-mono text-[10px] text-muted-foreground">
                  {diff.addedFields.map((f) => (
                    <span key={f}>+{f}</span>
                  ))}
                </div>
              )}
              {/* Overrides: the row existed, the overlay redefined it. */}
              {diff.changedFields.length > 0 && (
                <div className="mt-1 flex flex-wrap gap-x-2 gap-y-0.5 font-mono text-[10px] text-sky-600/90 dark:text-sky-400/90">
                  {diff.changedFields.map((f) => (
                    <span key={f}>~{f}</span>
                  ))}
                </div>
              )}
              {/* Removals get no focus affordance — the node they name is
                  gone from the graph, so there is nothing to focus. */}
              {(diff.removedTypes.length > 0 || diff.removedFields.length > 0) && (
                <div className="mt-1 flex flex-wrap gap-x-2 gap-y-0.5 font-mono text-[10px] text-red-600/80 dark:text-red-400/80">
                  {diff.removedTypes.map((t) => (
                    <span key={t}>−{t}</span>
                  ))}
                  {diff.removedFields.map((f) => (
                    <span key={f}>−{f}</span>
                  ))}
                </div>
              )}
            </div>
          ) : null}

          {warnings.length > 0 && (
            <div className="bg-amber-500/10 px-3 py-2">
              <div className="text-[10px] font-semibold uppercase tracking-wider text-amber-600 dark:text-amber-500">
                {warnings.length} warning{warnings.length !== 1 ? "s" : ""}
              </div>
              <ul className="mt-0.5 space-y-0.5 font-mono text-[10px] leading-snug text-amber-700/90 dark:text-amber-400/90">
                {warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/** Whole days between the end of the `until` day and now.
 *  Positive → overdue by that many days; negative → that many days left. */
function daysFromNow(until: string): number {
  const end = Date.parse(`${until}T23:59:59.999`);
  if (!Number.isFinite(end)) return 0;
  return Math.floor((Date.now() - end) / 86_400_000);
}

function groupByType(fields: UntilField[]) {
  const map = new Map<string, UntilField[]>();
  for (const f of fields) {
    if (!map.has(f.typeId)) map.set(f.typeId, []);
    map.get(f.typeId)!.push(f);
  }
  return [...map.entries()].map(([typeId, list]) => ({ typeId, list }));
}

type UntilVariant = "expired" | "upcoming" | "undated";

// Per-section styling: banner, field text/strike, date chip, focus tint.
const UNTIL_VARIANT: Record<
  UntilVariant,
  { title: string; banner: string; name: string; date: string; focus: string }
> = {
  expired: {
    title: "Expired",
    banner: "bg-red-500/10 text-red-600 dark:text-red-400",
    name: "text-red-600 decoration-red-500/50 dark:text-red-400",
    date: "text-red-500",
    focus: "bg-red-500/10",
  },
  upcoming: {
    title: "Upcoming",
    banner: "bg-amber-500/10 text-amber-600 dark:text-amber-400",
    name: "text-amber-700 decoration-amber-500/50 dark:text-amber-300",
    date: "text-amber-500",
    focus: "bg-amber-500/10",
  },
  undated: {
    title: "Deprecated",
    banner: "bg-slate-500/10 text-slate-600 dark:text-slate-400",
    name: "text-muted-foreground decoration-muted-foreground/50",
    date: "",
    focus: "bg-secondary",
  },
};

/** The "Deprecated" tab: expired / upcoming (dated) sections plus an
 *  undated section for deprecations with no `[until …]` sunset date. */
function UntilPanel({
  expired,
  upcoming,
  deprecated,
  focusId,
  onSelect,
}: {
  expired: UntilField[];
  upcoming: UntilField[];
  deprecated: UntilField[];
  focusId: string | null;
  onSelect: (f: UntilField) => void;
}) {
  return (
    <div className="min-h-0 flex-1 overflow-auto [scrollbar-width:none] [&_::-webkit-scrollbar]:w-0">
      {expired.length > 0 && (
        <UntilSection variant="expired" fields={expired} focusId={focusId} onSelect={onSelect} />
      )}
      {upcoming.length > 0 && (
        <UntilSection variant="upcoming" fields={upcoming} focusId={focusId} onSelect={onSelect} />
      )}
      {deprecated.length > 0 && (
        <UntilSection variant="undated" fields={deprecated} focusId={focusId} onSelect={onSelect} />
      )}
    </div>
  );
}

function UntilSection({
  variant,
  fields,
  focusId,
  onSelect,
}: {
  variant: UntilVariant;
  fields: UntilField[];
  focusId: string | null;
  onSelect: (f: UntilField) => void;
}) {
  const grouped = useMemo(() => groupByType(fields), [fields]);
  const v = UNTIL_VARIANT[variant];

  return (
    <section>
      {/* Section banner — distinguishes expired / upcoming / undated. */}
      <div
        className={cn(
          "sticky top-0 z-10 flex items-center gap-1.5 px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider backdrop-blur",
          v.banner,
        )}
      >
        {v.title}
        <span className="opacity-70">({fields.length})</span>
      </div>
      {grouped.map(({ typeId, list }) => {
        const style = KIND_STYLES[list[0]!.typeKind];
        return (
          <div key={typeId}>
            <div className="flex items-center gap-1.5 px-3 py-1 text-[10px]">
              <Badge className={cn("shrink-0 px-1.5 py-0 text-[9px] leading-4", style.badge)}>
                {style.label}
              </Badge>
              <span className="truncate font-mono text-muted-foreground">{list[0]!.typeName}</span>
              <span className="opacity-60">({list.length})</span>
            </div>
            <ul>
              {list.map((f) => {
                let meta: string;
                if (variant === "undated") {
                  meta = f.deprecationReason ?? "deprecated";
                } else {
                  const days = daysFromNow(f.until!);
                  const when =
                    variant === "expired"
                      ? days > 0 ? `${days.toLocaleString()}d overdue` : "overdue"
                      : days < 0 ? `in ${Math.abs(days).toLocaleString()}d` : "due today";
                  meta = f.deprecationReason
                    ? `${when} · ${stripUntil(f.deprecationReason)}`
                    : when;
                }
                return (
                  <li key={`${f.typeId}.${f.fieldName}.${f.fieldIndex}`}>
                    <button
                      type="button"
                      onClick={() => onSelect(f)}
                      className={cn(
                        "flex w-full flex-col gap-0.5 px-3 py-1.5 text-left transition-colors",
                        focusId === f.typeId ? v.focus : "hover:bg-secondary/60",
                      )}
                    >
                      <span className="flex w-full items-center gap-2 font-mono text-xs">
                        <span className={cn("truncate line-through", v.name)}>{f.fieldName}</span>
                        {f.until && (
                          <span className={cn("ml-auto shrink-0 font-mono text-[10px]", v.date)}>
                            {f.until}
                          </span>
                        )}
                      </span>
                      <span className="text-[10px] text-muted-foreground">{meta}</span>
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        );
      })}
    </section>
  );
}

/** Drops the leading `[until …]` marker from a reason for display —
 *  the date is already shown as its own chip. */
function stripUntil(reason: string): string {
  return reason.replace(/\[\s*until\s+\d{4}-\d{2}-\d{2}\s*\]\s*/i, "").trim();
}
