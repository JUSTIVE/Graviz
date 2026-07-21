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
import { useSchema, type UntilField } from "@/lib/schema-context";
import type { GraphNodeData } from "@/lib/sdl-to-graph";
import { cn } from "@/lib/utils";

type Mode = "reachable" | "orphaned" | "until";

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
    upcomingFields,
    deprecatedFields,
    pushFocus,
    popTo,
    pinnedField,
    setPinnedField,
  } = useSchema();
  const navigate = useNavigate();
  const [mode, setMode] = useState<Mode>("reachable");
  const [orphanFocus, setOrphanFocus] = useState<string | null>(null);
  const [untilFocus, setUntilFocus] = useState<string | null>(null);

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
          <TreePanel />
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

      <section
        className={cn(
          "relative min-w-0 flex-1 overflow-hidden lg:min-h-[500px]",
          // On mobile the canvas hides behind the full-screen tree while
          // the sidebar is open; it returns when the sidebar collapses.
          !collapsed && !isLg && "hidden",
        )}
      >
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
        />
      </section>
    </div>
  );
}

type Tone = "sky" | "amber" | "red";

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
