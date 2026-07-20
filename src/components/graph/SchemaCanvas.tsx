import { Application, Container, Geometry, Graphics, Mesh, NineSliceSprite, Shader, Sprite, Texture, TilingSprite, UniformGroup } from "pixi.js";
import {
  buildNodeTextMesh,
  flushSdfAtlas,
  updateTextZoom,
  type TextRun,
} from "./sdf-text";
import { ArrowRight, ChevronDown, ChevronUp, Filter, History, Loader2, Microscope, Trash2, X } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { BezierSegment, LayoutResult } from "@/lib/layout";
import {
  LayoutOrchestrator,
  defaultPoolSize,
  type OrchestratorRequest,
  type OrchestratorTimings,
} from "@/lib/layout-orchestrator";
import type { GraphEdgeData, GraphNodeData, NodeKind } from "@/lib/sdl-to-graph";
import { useSchema } from "@/lib/schema-context";
import { isUntilExpired } from "@/lib/until";
import { useTheme } from "@/lib/theme";
import { applyTooltipStyle, tooltipStyle } from "@/lib/tooltip-pos";
import { cn } from "@/lib/utils";
import {
  HEADER_H,
  IMPL_SECTION_GAP,
  KIND_COLORS,
  KIND_STYLES,
  NODE_NAME_FONT,
  ROW_H,
  TOP_BODY_PAD,
  estimateNodeHeight,
  estimateNodeWidth,
  headerHFor,
  rowHFor,
} from "./node-style";

/**
 * Pixi.js v8 schema graph renderer.
 *
 * Scene graph:
 *   Application.stage
 *    ├── gridTiling (TilingSprite) — dot grid, screen-space
 *    └── world (Container) — pan/zoom transform
 *         ├── edgeTileContainer (Container) — batched per-tile edge+arrow Graphics
 *         ├── nodeContainer (Container) — one Sprite per node
 *         ├── hoverGraphics (Graphics) — field row highlight
 *         └── focusGraphics (Graphics) — focus ring + hover ring
 */

interface LaidNode {
  id: string;
  data: GraphNodeData;
  cx: number;
  cy: number;
  w: number;
  h: number;
  /** Per-node row height. Equals the base ROW_H when the
   *  "Show descriptions" toggle is off, or ROW_H_WITH_DESC when on
   *  so each field row reserves space for an inline description
   *  line beneath its name. */
  rowH: number;
  /** Per-node header height. Equals HEADER_H by default, or
   *  HEADER_H_WITH_DESC when "Show descriptions" is on so the
   *  type's own description fits below the type name. */
  headerH: number;
}

interface Point {
  x: number;
  y: number;
}

interface LaidEdge {
  sourceId: string;
  targetId: string;
  kind: GraphEdgeData["kind"];
  nullable: boolean;
  /** Human-readable label — for field/arg edges this is the field
   *  name, for implements/union edges the relationship word. Surfaced
   *  in the edge hover tooltip. */
  label?: string;
  /** Per-edge opacity multiplier (1 = full). Edges incident to a
   *  hub node (in-degree or out-degree ≥ HUB_FADE_DEGREE) get
   *  HUB_FADE_ALPHA so hub fan-outs don't drown the canvas. */
  hubFade?: number;
  start: Point;
  segments: BezierSegment[];
  arrowTip?: Point;
  bbox: { minX: number; minY: number; maxX: number; maxY: number };
}

/** A node whose in-degree OR out-degree reaches this is treated as
 *  a hub. Set just above the typical Relay `Node` interface (1 per
 *  implementor) so normal types stay at full opacity. */
const HUB_FADE_DEGREE = 50;
/** Alpha multiplier for edges incident to a hub node. */
const HUB_FADE_ALPHA = 0.3;

interface Props {
  nodes: GraphNodeData[];
  edges: GraphEdgeData[];
  focusId?: string | null;
  rootId?: string | null;
  onNavigate?: (typeId: string) => void;
  onClearFocus?: () => void;
  /** When true, the canvas's top-left overlay controls slide down to
   *  clear room for an external control (the sidebar-expand button that
   *  appears when the sidebar is collapsed). */
  leftControlsInset?: boolean;
}

interface EdgeGroupSpec {
  color: string;
  colorHex: number;
  /** Alpha multiplier applied on top of the dim/active opacity. Used
   *  in place of dash patterns to softly distinguish edge kinds that
   *  share a hue with another group (e.g. nullable vs non-null field
   *  edges, both blue). */
  alphaScale: number;
  dim: LaidEdge[];
  active: LaidEdge[];
}

interface EdgeGroups {
  groups: EdgeGroupSpec[];
  dimNodeIds: Set<string>;
}

interface SpriteCtx {
  cardColor: string;
  fgColor: string;
  mutedFg: string;
}

const BUILTIN_SCALARS = new Set(["String", "Int", "Float", "Boolean", "ID"]);
const CLICK_DRAG_THRESHOLD = 4;
const EMPTY_LAYOUT: LayoutResult = { nodes: [], edgePaths: [] };
const CULL_PAD = 100;
const MONO = "ui-monospace, SFMono-Regular, Menlo, monospace";

// Edge tiling. Large schemas tessellate millions of vertices across
// their 10k+ edges; a single monolithic `Graphics` for all of them
// blows through mobile GPU budgets. We partition world-space into
// TILE_SIZE cells, build per-tile Graphics lazily when the tile enters
// the viewport, and destroy them once they've been off-screen long
// enough. Edges whose bbox spans multiple tiles are registered in each
// overlapping tile — duplication is bounded (typically 1–2×) because
// dot's layout keeps connected nodes spatially close.
const TILE_SIZE = 2048;
const TILE_EVICT_FRAMES = 180;
const TILE_VIEW_PADDING = 256;

// Sprite viewport management. Mirrors the edge-tile strategy: only
// sprites currently inside the padded viewport get real textures;
// off-screen sprites show a tinted placeholder so texture memory
// scales with visible area instead of total node count.
const SPRITE_VIEW_PADDING = 200;
const SPRITE_EVICT_FRAMES = 180;

// Quiet-period gate for GPU uploads. While the user is actively
// panning or zooming we keep sprites on their tinted placeholder and
// defer tile/texture builds — a fast pan on a large schema can queue
// hundreds of `texImage2D` calls per second and crash mobile GPU
// drivers. Once the view has been stable for this many milliseconds
// we resume progressive builds.
const MOTION_SETTLE_MS = 150;

interface EdgeTile {
  key: string;
  col: number;
  row: number;
  /** Per-group edge lists (indexed like EDGE_GROUP_DEFS). Deliberately
   *  independent of the focus dim/active partition — tiles are static
   *  geometry built once per layout, and focus dimming is applied via
   *  container alpha + a separate active-edge overlay so navigating
   *  never re-tessellates the whole edge set. */
  groupLists: LaidEdge[][];
  /** Edge Graphics broken into ≤ EDGES_PER_BATCH-edge sub-batches.
   *  Each Graphics has bounded vertex count so a single tile never
   *  uploads a multi-MB vertex buffer in one frame. */
  edgeBatches: Container[];
  /** Number of batches built so far. The remaining batches are
   *  appended progressively across subsequent frames. */
  builtBatches: number;
  /** Total batches planned. -1 means "not yet computed" — gets filled
   *  the first time the tile becomes visible (so we don't pay the
   *  planning cost for off-screen tiles). */
  totalBatches: number;
  lastSeenFrame: number;
}

// Max edges packed into a single Graphics. Aggressively small so a
// single failed `stroke()` upload can never overrun the WebGL
// scratch buffer — long polyline edges in a hub-heavy schema can
// pack ~1k triangles per edge after tessellation, and at 16 edges
// per batch we stay well under any per-draw limit even on low-end
// integrated GPUs. The cost is more Graphics objects (and thus more
// draw calls), which Pixi's batcher coalesces back to a similar
// number of GPU submissions per frame.
const EDGES_PER_BATCH = 16;
// Max new batches built per animation frame, summed across all
// tiles. Increased proportionally to the smaller batch size so
// fill-in speed (edges per frame) stays the same as before.
const TILE_BATCH_BUDGET_PER_FRAME = 24;

// LOD tiers — thresholds tuned conservatively so the full-text
// rendering survives further zoom-outs. Drop to the bar / chrome
// placeholders only when the user is genuinely far enough away that
// text would be illegible anyway.
type SpriteLOD = "full" | "bar" | "chrome";
const LOD_FULL = 0.06;
const LOD_BAR = 0.02;
// Zoom level below which individual field-row clicks stop being a
// useful target — text is too small for precise pointing even
// though the sprite still uses the full-LOD texture. Below this the
// node-name tooltip appears and a node click frames the node
// (instead of treating the click as a field hit).
const FIELD_CLICK_MIN_ZOOM = 0.35;
// Hysteresis: once inside a tier, require a slightly larger excursion
// before exiting. Prevents oscillation (and its sprite rebuild cost)
// when the user parks their zoom right on a boundary.
const LOD_HYSTERESIS = 0.015;

function computeLOD(viewK: number, prev: SpriteLOD): SpriteLOD {
  if (prev === "full") {
    if (viewK >= LOD_FULL - LOD_HYSTERESIS) return "full";
    if (viewK >= LOD_BAR) return "bar";
    return "chrome";
  }
  if (prev === "bar") {
    if (viewK >= LOD_FULL) return "full";
    if (viewK >= LOD_BAR - LOD_HYSTERESIS) return "bar";
    return "chrome";
  }
  if (viewK >= LOD_FULL) return "full";
  if (viewK >= LOD_BAR) return "bar";
  return "chrome";
}

const BAR_NAME_FRACS = [0.62, 0.50, 0.71, 0.55, 0.44, 0.68];
const BAR_FIELD_FRACS = [0.44, 0.36, 0.52, 0.38, 0.46, 0.32];
const BAR_TYPE_FRACS = [0.24, 0.30, 0.20, 0.27, 0.22, 0.28];

// Conservative DPR caps that survive 1,400-node schemas on every
// device we care about. Per-tab GPU budget in Chrome lands around
// 500 MB even on desktop; at bar DPR=2 a 1,400-sprite schema plus
// framebuffer and internal Pixi state overruns that on an active
// pan and the renderer process dies ("Aw, Snap!"). Bar LOD is just
// fake-bar hints, DPR 1 is indistinguishable at zoom.
//
// Full LOD scales with the actual zoom so a node's texture always
// matches the physical pixels it covers on screen: a node displayed
// at k×w CSS px covers k×w×renderDpr device px (renderDpr = the
// renderer's resolution, capped at 2 — texels beyond that can't be
// displayed anyway). Bucketed to a ×2 ladder {0.5, 1, 2, 4, 8} with
// √2 midpoint thresholds so ordinary zoom jitter doesn't re-key
// textures; crossing a boundary rebuilds visible textures (text
// progressively sharpens as you zoom in), same tradeoff as a LOD
// tier crossing. Extreme node sizes are still clamped by
// fitDprToMaxTexture against the GPU's max texture dimension, and
// the in-view node count shrinks as k grows, so high buckets stay
// cheap in aggregate.
function spriteDprForLod(lod: SpriteLOD, viewK: number): number {
  const renderDpr =
    typeof window !== "undefined" ? Math.min(2, Math.max(1, window.devicePixelRatio || 1)) : 1;
  if (lod === "chrome") return 1;
  if (lod === "bar") return 1;
  const eff = viewK * renderDpr;
  if (eff >= 5.6) return 8;
  if (eff >= 2.8) return 4;
  if (eff >= 1.4) return 2;
  if (eff >= 0.7) return 1;
  return 0.5;
}

// Probe WebGL `MAX_TEXTURE_SIZE` once so we can cap per-node textures
// to whatever the GPU/browser actually accepts. Firefox in particular
// will silently drop the upload (rendering nothing) when a sprite
// texture exceeds this — a 250-value enum at full LOD reaches
// ~7000 px tall at DPR=2, which clears Chrome's typical 16384 ceiling
// but blows past Firefox's commonly-reported 8192 on integrated GPUs.
// Subtract a small margin so we stay well under whatever Pixi's own
// pre-upload allocation also reserves.
let _maxTexDimCache: number | null = null;
function getMaxTextureDim(): number {
  if (_maxTexDimCache != null) return _maxTexDimCache;
  if (typeof document === "undefined") return 4096;
  try {
    const c = document.createElement("canvas");
    const gl =
      (c.getContext("webgl2") as WebGL2RenderingContext | null) ??
      (c.getContext("webgl") as WebGLRenderingContext | null);
    if (gl) {
      const max = gl.getParameter(gl.MAX_TEXTURE_SIZE) as number;
      if (typeof max === "number" && max > 0) {
        _maxTexDimCache = Math.max(2048, Math.floor(max * 0.95));
        return _maxTexDimCache;
      }
    }
  } catch {
    // ignore — fall through to fallback
  }
  _maxTexDimCache = 4096;
  return _maxTexDimCache;
}

// Returns a DPR ≤ baseDpr such that w*dpr and h*dpr both fit under the
// GPU's max texture dimension. The sprite still displays at w×h (Pixi
// scales the texture to the sprite bounds), so the only visible cost is
// slightly less crisp text on extremely tall nodes — strictly better
// than rendering nothing.
function fitDprToMaxTexture(w: number, h: number, dpr: number): number {
  const limit = getMaxTextureDim();
  const worst = Math.max(w, h);
  if (worst * dpr <= limit) return dpr;
  return Math.max(0.5, limit / worst);
}

// LOD-aware live texture cache caps. "bar" is cheap (48 KB/texture at
// DPR=1) so we let a 1,400-node grid fully populate; "full" costs 4×
// more per texture so we keep the ceiling tight. When the cap is hit
// the drain simply stops — remaining sprites stay on the tint
// placeholder until off-screen sprites evict and free room. We never
// evict currently-in-view sprites to make space, because on a
// schema whose viewport contains more sprites than the cap allows
// that would become a permanent build→evict→rebuild churn and crash
// the GPU driver within a couple of seconds.
const MAX_TEXTURE_CACHE_BAR = 1600;
const MAX_TEXTURE_CACHE_FULL = 400;

function maxTextureCacheFor(lod: SpriteLOD, dpr: number): number {
  if (lod === "bar") return MAX_TEXTURE_CACHE_BAR;
  // The full-LOD cap was tuned for DPR-2 textures; other buckets cost
  // dpr²-proportionally more/less memory, so scale the entry count
  // inversely (bounded by the bar cap so object count stays sane,
  // floored so the handful of nodes visible at deep zoom — where the
  // high-DPR buckets apply — never starves the build queue).
  return Math.min(
    MAX_TEXTURE_CACHE_BAR,
    Math.max(32, Math.round(MAX_TEXTURE_CACHE_FULL * (2 / dpr) ** 2)),
  );
}

function getComputedCssVar(name: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  const val = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return val || fallback;
}

// Memoized by input string — the ticker asks for the same handful of
// colors every frame, and the hsl()/rgb() fallback below allocates a
// canvas + getImageData per call, which is far too expensive to run
// per frame.
const cssColorHexCache = new Map<string, number>();

function cssColorToHex(color: string): number {
  const cached = cssColorHexCache.get(color);
  if (cached !== undefined) return cached;
  const hex = computeCssColorHex(color);
  cssColorHexCache.set(color, hex);
  return hex;
}

function computeCssColorHex(color: string): number {
  // Handle #rrggbb
  if (color.startsWith("#") && color.length === 7) {
    return parseInt(color.slice(1), 16);
  }
  // Handle #rgb
  if (color.startsWith("#") && color.length === 4) {
    const r = color[1]!;
    const g = color[2]!;
    const b = color[3]!;
    return parseInt(r + r + g + g + b + b, 16);
  }
  // Handle hsl(...) and rgb(...) via a canvas trick
  if (typeof document !== "undefined") {
    const canvas = document.createElement("canvas");
    canvas.width = 1;
    canvas.height = 1;
    const ctx = canvas.getContext("2d");
    if (ctx) {
      ctx.fillStyle = color;
      ctx.fillRect(0, 0, 1, 1);
      const d = ctx.getImageData(0, 0, 1, 1).data;
      return ((d[0]! << 16) | (d[1]! << 8) | d[2]!);
    }
  }
  return 0xffffff;
}

// measureText cache
const typeWidthCache = new Map<string, number>();
const fitTextCache = new Map<string, string>();

function cachedTextWidth(ctx: CanvasRenderingContext2D, text: string): number {
  let w = typeWidthCache.get(text);
  if (w !== undefined) return w;
  w = ctx.measureText(text).width;
  typeWidthCache.set(text, w);
  return w;
}

// Module-level measurement ctx pinned to the field-type font so the
// hit-test can locate the right-aligned return-type label without
// touching a rendering canvas. Reuses `typeWidthCache` since the cache
// is keyed by text and the field-type font is the only one cached.
let fieldTypeMeasureCtx: CanvasRenderingContext2D | null = null;
function fieldTypeTextWidth(text: string): number {
  if (!fieldTypeMeasureCtx) {
    if (typeof document === "undefined") return 0;
    const c = document.createElement("canvas");
    const ctx = c.getContext("2d");
    if (!ctx) return 0;
    ctx.font = `10px ${MONO}`;
    fieldTypeMeasureCtx = ctx;
  }
  return cachedTextWidth(fieldTypeMeasureCtx, text);
}

function fitText(ctx: CanvasRenderingContext2D, s: string, maxWidth: number): string {
  const cacheKey = `${s}|${maxWidth}`;
  const cached = fitTextCache.get(cacheKey);
  if (cached !== undefined) return cached;

  if (ctx.measureText(s).width <= maxWidth) {
    fitTextCache.set(cacheKey, s);
    return s;
  }
  const ellipsis = "…";
  let lo = 0;
  let hi = s.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    const cand = s.slice(0, mid) + ellipsis;
    if (ctx.measureText(cand).width <= maxWidth) lo = mid;
    else hi = mid - 1;
  }
  const result = lo > 0 ? s.slice(0, lo) + ellipsis : ellipsis;
  fitTextCache.set(cacheKey, result);
  return result;
}

// ─── SDF text mode (hybrid renderer experiment) ──────────────────────
// When on, full-LOD card textures are baked *without text* at a fixed
// DPR (chrome is flat color, it doesn't need zoom-bucket sharpening)
// and the text is rendered as SDF glyph quad meshes on a layer above
// the sprites — crisp at every zoom with zero rebuilds. Toggle off for
// A/B comparison with `?sdf=0`.
const SDF_TEXT_ENABLED =
  typeof window !== "undefined" &&
  new URLSearchParams(window.location.search).get("sdf") !== "0";
const SDF_CHROME_DPR =
  typeof window !== "undefined" ? Math.min(2, Math.max(1, window.devicePixelRatio || 1)) : 1;

/** Parsed subset of a ctx.font string, cached by the raw string. */
const fontParseCache = new Map<string, { px: number; weight: number; italic: boolean }>();
function parseCtxFont(font: string): { px: number; weight: number; italic: boolean } {
  let p = fontParseCache.get(font);
  if (p) return p;
  const px = parseFloat(/(\d+(?:\.\d+)?)px/.exec(font)?.[1] ?? "10");
  const weight = parseInt(/(?:^|\s)([1-9]00)(?:\s)/.exec(font)?.[1] ?? "400", 10);
  const italic = /(?:^|\s)italic(?:\s|$)/.test(font);
  p = { px, weight, italic };
  fontParseCache.set(font, p);
  return p;
}

/**
 * fillText or capture: when `sink` is set the run is recorded (SDF
 * text mode — the glyphs will be rendered by the SDF mesh layer) and
 * nothing is painted; otherwise this is a plain ctx.fillText. Reads
 * font / fillStyle / globalAlpha from the ctx so call sites keep their
 * existing state-setting code as the single source of truth.
 */
function paintText(
  ctx: CanvasRenderingContext2D,
  sink: TextRun[] | null,
  text: string,
  x: number,
  y: number,
) {
  if (!sink) {
    ctx.fillText(text, x, y);
    return;
  }
  const { px, weight, italic } = parseCtxFont(ctx.font);
  sink.push({
    text,
    x,
    y,
    px,
    weight,
    italic,
    color: typeof ctx.fillStyle === "string" ? ctx.fillStyle : "#ffffff",
    alpha: ctx.globalAlpha,
  });
}

function roundRect(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + w - r, y);
  ctx.arcTo(x + w, y, x + w, y + r, r);
  ctx.lineTo(x + w, y + h - r);
  ctx.arcTo(x + w, y + h, x + w - r, y + h, r);
  ctx.lineTo(x + r, y + h);
  ctx.arcTo(x, y + h, x, y + h - r, r);
  ctx.lineTo(x, y + r);
  ctx.arcTo(x, y, x + r, y, r);
  ctx.closePath();
}

function roundRectTopOnly(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  w: number,
  h: number,
  r: number,
) {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.lineTo(x + w - r, y);
  ctx.arcTo(x + w, y, x + w, y + r, r);
  ctx.lineTo(x + w, y + h);
  ctx.lineTo(x, y + h);
  ctx.lineTo(x, y + r);
  ctx.arcTo(x, y, x + r, y, r);
  ctx.closePath();
}

function drawColoredType(
  ctx: CanvasRenderingContext2D,
  typeStr: string,
  rightX: number,
  y: number,
  /** Primitive (built-in scalar) types get a desaturated muted amber
   *  to visually de-emphasize them against custom types that the
   *  user typically cares more about. */
  primitive: boolean = false,
  /** Extra alpha multiplier — used to fade the type when the row is
   *  deprecated, matching the fade applied to the field name. */
  baseAlpha: number = 1,
  /** When set, overrides the type color entirely (used to paint the
   *  type red on expired rows). */
  colorOverride?: string,
  sink: TextRun[] | null = null,
) {
  const w = cachedTextWidth(ctx, typeStr);
  ctx.fillStyle = colorOverride ?? (primitive ? "#b08c5a" : "#f59e0b");
  ctx.globalAlpha = (colorOverride ? 1 : primitive ? 0.7 : 1) * baseAlpha;
  paintText(ctx, sink, typeStr, rightX - w, y);
  ctx.globalAlpha = 1;
}

/** Red used to flag fields/values whose `[until …]` sunset date has
 *  passed — they should have been removed and are now overdue. */
const EXPIRED_COLOR = "#ef4444"; // red-500
const RELAY_COLOR = "#F26A03";
const RELAY_SVG_PATH = new Path2D(
  "M2.264 4.937A2.264 2.264 0 1 0 4.456 7.77h10.339a1.792 1.792 0 0 1 0 3.583h-5.73a3.037 3.037 0 0 0-3.034 3.033a3.036 3.036 0 0 0 3.033 3.033h10.494a2.264 2.264 0 1 0 0-1.242H9.064a1.793 1.793 0 0 1-1.791-1.791c0-.988.803-1.792 1.791-1.792h5.73a3.036 3.036 0 0 0 3.034-3.033a3.036 3.036 0 0 0-3.033-3.033H4.427a2.265 2.265 0 0 0-2.163-1.592",
);

function drawRelayIcon(ctx: CanvasRenderingContext2D, cx: number, cy: number) {
  const scale = 8 / 24;
  ctx.save();
  ctx.fillStyle = RELAY_COLOR;
  ctx.globalAlpha = 0.85;
  ctx.translate(cx - 12 * scale, cy - 12 * scale);
  ctx.scale(scale, scale);
  ctx.fill(RELAY_SVG_PATH);
  ctx.restore();
}

/**
 * Card-local Y geometry of the trailing implements / member-of-union
 * sections. Mirrors drawNodeSprite's full-tier text layout exactly
 * (including the +2 body offset, section gaps, and in-band centering)
 * so the hit-test and hover overlay land on the same pixels the
 * painter used. Section rows always use the tight ROW_H pitch — they
 * never carry description lines.
 */
function trailingSectionGeom(n: LaidNode): {
  /** Violet wash band (implements). Zero-height when no interfaces. */
  ifaceBandTop: number;
  ifaceBandBottom: number;
  /** Amber wash band (member-of-union). Runs to the card bottom. */
  unionBandTop: number;
  ifaceRowsTop: number;
  ifaceBlockH: number;
  unionRowsTop: number;
  unionBlockH: number;
} {
  const fields = n.data.fields?.length ?? 0;
  const ifaces = n.data.interfaces?.length ?? 0;
  const unions =
    n.data.kind === "Object" ? (n.data.memberOfUnions?.length ?? 0) : 0;
  const implGap = ifaces > 0 && fields > 0 ? IMPL_SECTION_GAP : 0;
  const unionGap = unions > 0 && ifaces === 0 && fields > 0 ? IMPL_SECTION_GAP : 0;
  const washTop =
    n.headerH + TOP_BODY_PAD + fields * n.rowH - 2 + implGap + unionGap;
  const ifaceBlockH = ifaces * ROW_H;
  const unionBlockH = unions * ROW_H;
  // Leftover space below the section rows (bottom pad + rounding).
  // With both sections present it is split evenly between the two
  // bands so equal row counts render equal band heights; a lone
  // section absorbs all of it.
  const extra = Math.max(0, n.h - washTop - ifaceBlockH - unionBlockH);
  const ifaceBandTop = washTop;
  const ifaceBandBottom =
    ifaces > 0
      ? unions > 0
        ? washTop + ifaceBlockH + Math.floor(extra / 2)
        : n.h
      : washTop;
  const unionBandTop = ifaces > 0 ? ifaceBandBottom : washTop;
  const ifaceRowsTop =
    ifaceBandTop +
    Math.max(0, Math.floor((ifaceBandBottom - ifaceBandTop - ifaceBlockH) / 2));
  const unionRowsTop =
    unionBandTop + Math.max(0, Math.floor((n.h - unionBandTop - unionBlockH) / 2));
  return {
    ifaceBandTop,
    ifaceBandBottom,
    unionBandTop,
    ifaceRowsTop,
    ifaceBlockH,
    unionRowsTop,
    unionBlockH,
  };
}

function bodyRowCount(n: LaidNode): number {
  const d = n.data;
  if (d.kind === "Enum") return (d.values ?? []).length;
  if (d.kind === "Union") return (d.members ?? []).length;
  if (d.kind === "Scalar") return 1;
  return (
    (d.fields ?? []).length +
    (d.interfaces ?? []).length +
    (d.memberOfUnions ?? []).length
  );
}

function drawNodeSprite(
  ctx: CanvasRenderingContext2D,
  n: LaidNode,
  { cardColor, fgColor, mutedFg }: SpriteCtx,
  lod: SpriteLOD,
  /** SDF text mode: capture text runs here instead of rasterizing
   *  them into the card texture (full LOD only). */
  sink: TextRun[] | null = null,
) {
  const w = n.w;
  const h = n.h;
  const rowH = n.rowH;
  const headerH = n.headerH;
  const showDesc = rowH !== ROW_H;
  const color = KIND_COLORS[n.data.kind];

  if (lod === "chrome") {
    roundRect(ctx, 0, 0, w, h, 6);
    ctx.fillStyle = color;
    ctx.fill();
    return;
  }

  roundRect(ctx, 0, 0, w, h, 6);
  ctx.fillStyle = cardColor;
  ctx.fill();
  ctx.strokeStyle = color;
  ctx.lineWidth = 1.25;
  ctx.globalAlpha = 0.75;
  ctx.stroke();
  ctx.globalAlpha = 1;

  roundRectTopOnly(ctx, 0, 0, w, headerH, 6);
  ctx.fillStyle = color;
  ctx.fill();

  ctx.strokeStyle = color;
  ctx.globalAlpha = 0.4;
  ctx.lineWidth = 0.75;
  ctx.beginPath();
  ctx.moveTo(0, headerH);
  ctx.lineTo(w, headerH);
  ctx.stroke();
  ctx.globalAlpha = 1;

  // Trailing-section backgrounds — a violet wash for the `implements`
  // rows and an amber wash (Union kind color) for the "member of
  // union" rows, so both read as sub-sections distinct from the field
  // list.
  if (n.data.kind === "Object" || n.data.kind === "Interface") {
    const interfaceCount = n.data.interfaces?.length ?? 0;
    const unionCount =
      n.data.kind === "Object" ? (n.data.memberOfUnions?.length ?? 0) : 0;
    const washSection = (top: number, bottom: number, color: string) => {
      // Clip to the rounded card shape so the wash doesn't spill
      // past the bottom corner curves.
      ctx.save();
      roundRect(ctx, 0, 0, w, h, 6);
      ctx.clip();
      ctx.fillStyle = color;
      ctx.globalAlpha = 0.1;
      ctx.fillRect(0, top, w, bottom - top);
      ctx.globalAlpha = 1;
      ctx.restore();
      // Thin divider above the section.
      ctx.strokeStyle = color;
      ctx.globalAlpha = 0.4;
      ctx.lineWidth = 0.5;
      ctx.beginPath();
      ctx.moveTo(0, top);
      ctx.lineTo(w, top);
      ctx.stroke();
      ctx.globalAlpha = 1;
    };
    // Band bounds come from the shared geometry helper — the same
    // source the hit-test and hover overlay use.
    const geom = trailingSectionGeom(n);
    if (interfaceCount > 0) {
      washSection(geom.ifaceBandTop, geom.ifaceBandBottom, KIND_COLORS.Interface);
    }
    if (unionCount > 0) {
      washSection(geom.unionBandTop, h, KIND_COLORS.Union);
    }
  }

  if (lod === "bar") {
    const avail = w - 16;
    const nFrac = BAR_NAME_FRACS[0]!;
    ctx.fillStyle = "#ffffff";
    ctx.globalAlpha = 0.55;
    roundRect(ctx, 8, 23, avail * nFrac, 5, 2.5);
    ctx.fill();
    ctx.globalAlpha = 1;

    const bodyY = headerH + TOP_BODY_PAD - 2;
    const rowCount = bodyRowCount(n);
    // Union member bars mirror the tight ROW_H pitch of the real rows.
    const barPitch = n.data.kind === "Union" ? ROW_H : rowH;
    for (let i = 0; i < rowCount; i++) {
      const fy = bodyY + i * barPitch + 3;
      const ff = BAR_FIELD_FRACS[i % BAR_FIELD_FRACS.length]!;
      const tf = BAR_TYPE_FRACS[i % BAR_TYPE_FRACS.length]!;
      const typeBarW = avail * tf;
      ctx.fillStyle = fgColor;
      ctx.globalAlpha = 0.35;
      roundRect(ctx, 10, fy, avail * ff, 4, 2);
      ctx.fill();
      ctx.fillStyle = "#f59e0b";
      ctx.globalAlpha = 0.45;
      roundRect(ctx, w - 10 - typeBarW, fy, typeBarW, 4, 2);
      ctx.fill();
      ctx.globalAlpha = 1;
    }
    return;
  }

  // full tier
  ctx.font = `600 9px ${MONO}`;
  ctx.fillStyle = "#ffffff";
  ctx.globalAlpha = 0.6;
  paintText(ctx, sink, n.data.kind.toUpperCase(), 8, 14);
  ctx.globalAlpha = 1;

  ctx.font = NODE_NAME_FONT;
  ctx.fillStyle = "#ffffff";
  paintText(ctx, sink, fitText(ctx, n.data.name, w - 16), 8, 30);

  // Type-level description rendered in the header when the toggle is
  // on. One regular line, white-on-color-header, left-aligned with
  // the type name above it, truncated to fit.
  if (showDesc && n.data.description?.trim()) {
    ctx.font = `9px ${MONO}`;
    ctx.fillStyle = "#ffffff";
    ctx.globalAlpha = 0.75;
    paintText(ctx, sink, fitText(ctx, n.data.description.replace(/\s+/g, " ").trim(), w - 16), 8, 42);
    ctx.globalAlpha = 1;
  }

  const bodyY = headerH + TOP_BODY_PAD - 2;
  const drawRowDesc = (desc: string | undefined | null, fy: number) => {
    if (!showDesc || !desc) return;
    ctx.save();
    ctx.font = `9px ${MONO}`;
    ctx.fillStyle = mutedFg;
    ctx.globalAlpha = 0.7;
    // Match the field-name's x=10 left margin so the description
    // sits flush under its row's name (no longer indented).
    paintText(
      ctx,
      sink,
      fitText(ctx, desc.replace(/\s+/g, " ").trim(), w - 20),
      10,
      fy + 11,
    );
    ctx.restore();
  };
  if (n.data.kind === "Enum") {
    ctx.font = `10px ${MONO}`;
    ctx.fillStyle = mutedFg;
    const values = n.data.values ?? [];
    for (let i = 0; i < values.length; i++) {
      const v = values[i]!;
      const fy = bodyY + i * rowH + 10;
      const expired = isUntilExpired(v.until);
      ctx.font = `10px ${MONO}`;
      ctx.fillStyle = expired ? EXPIRED_COLOR : mutedFg;
      paintText(ctx, sink, v.name, 10, fy);
      if (expired) {
        const nameW = ctx.measureText(v.name).width;
        ctx.strokeStyle = EXPIRED_COLOR;
        ctx.lineWidth = 0.75;
        ctx.beginPath();
        ctx.moveTo(10, fy - 3.5);
        ctx.lineTo(10 + nameW, fy - 3.5);
        ctx.stroke();
      }
      // Deprecated values without a description fall back to the
      // @deprecated reason so the row still gets an inline note.
      drawRowDesc(
        v.description ?? (v.isDeprecated ? v.deprecationReason : undefined),
        fy,
      );
    }
  } else if (n.data.kind === "Union") {
    // Member rows are Object types — paint them in the Object kind
    // color, no "|" prefix (the union header already says union).
    // Members carry no description line, so they always use the tight
    // ROW_H pitch, even in descriptions mode.
    ctx.font = `10px ${MONO}`;
    ctx.fillStyle = KIND_COLORS.Object;
    const members = n.data.members ?? [];
    for (let i = 0; i < members.length; i++) {
      paintText(ctx, sink, members[i]!, 10, bodyY + i * ROW_H + 10);
    }
  } else if (n.data.kind === "Scalar") {
    ctx.font = `italic 10px ${MONO}`;
    ctx.fillStyle = mutedFg;
    paintText(ctx, sink, "custom scalar", 10, bodyY + 10);
  } else {
    const fields = n.data.fields ?? [];
    ctx.font = `10px ${MONO}`;
    for (let i = 0; i < fields.length; i++) {
      const f = fields[i]!;
      const fy = bodyY + i * rowH + 10;
      const expired = isUntilExpired(f.until);
      // Expired rows render at full opacity in red; ordinary deprecated
      // rows keep the muted amber fade.
      const depAlpha = expired ? 1 : f.isDeprecated ? 0.4 : 1;
      ctx.font = `10px ${MONO}`;
      ctx.fillStyle = expired ? EXPIRED_COLOR : fgColor;
      ctx.globalAlpha = depAlpha;
      paintText(ctx, sink, f.name, 10, fy);
      // Defensive truncation: the width estimate should fit every
      // rendered type, but if a stale layout (or the min-width clamp)
      // leaves the row short, ellipse the type rather than letting it
      // pierce the card edge / overlap the name.
      const nameW = cachedTextWidth(ctx, f.name);
      const relayPad = f.isRelayConnection ? 20 : 0;
      const typeStr = fitText(
        ctx,
        f.type,
        Math.max(40, w - 20 - nameW - relayPad - 8),
      );
      if (f.isDeprecated) {
        const typeW = cachedTextWidth(ctx, typeStr);
        ctx.strokeStyle = expired ? EXPIRED_COLOR : fgColor;
        ctx.lineWidth = 0.75;
        ctx.beginPath();
        // Strikethrough name and type so the entire row reads as
        // deprecated, not just the field name.
        ctx.moveTo(10, fy - 3.5);
        ctx.lineTo(10 + nameW, fy - 3.5);
        ctx.moveTo(w - 10 - typeW, fy - 3.5);
        ctx.lineTo(w - 10, fy - 3.5);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
      if (f.isRelayConnection) {
        const typeW = cachedTextWidth(ctx, typeStr);
        const iconCx = w - 10 - typeW - 8;
        ctx.globalAlpha = depAlpha;
        drawRelayIcon(ctx, iconCx, fy - 2);
        ctx.globalAlpha = 1;
        ctx.font = `10px ${MONO}`;
      }
      drawColoredType(ctx, typeStr, w - 10, fy, BUILTIN_SCALARS.has(f.typeName), depAlpha, expired ? EXPIRED_COLOR : undefined, sink);
      drawRowDesc(
        f.description ?? (f.isDeprecated ? f.deprecationReason : undefined),
        fy,
      );
    }
    const interfaces = n.data.interfaces ?? [];
    const memberOfUnions =
      n.data.kind === "Object" ? (n.data.memberOfUnions ?? []) : [];
    // Section rows never carry descriptions — tight ROW_H pitch
    // regardless of descriptions mode. Row positions come from the
    // shared geometry helper (centered inside each wash band); the
    // "implements" / "|" prefixes are dropped — the band colors
    // already communicate the semantics.
    const geom = trailingSectionGeom(n);
    if (interfaces.length > 0) {
      ctx.font = `600 10px ${MONO}`;
      ctx.fillStyle = KIND_COLORS.Interface;
      for (let i = 0; i < interfaces.length; i++) {
        const fy = geom.ifaceRowsTop + i * ROW_H + 10;
        paintText(ctx, sink, fitText(ctx, interfaces[i]!, w - 20), 10, fy);
      }
    }
    if (memberOfUnions.length > 0) {
      ctx.font = `600 10px ${MONO}`;
      ctx.fillStyle = KIND_COLORS.Union;
      for (let i = 0; i < memberOfUnions.length; i++) {
        const fy = geom.unionRowsTop + i * ROW_H + 10;
        paintText(ctx, sink, fitText(ctx, memberOfUnions[i]!, w - 20), 10, fy);
      }
    }
  }
}

/**
 * Shared "chrome/bar LOD" placeholder texture for one node kind.
 * Small (256×128 DPR 1 = 128 KB) and reused across every sprite of
 * that kind — so a 1,400-node schema only ever needs 6 placeholder
 * uploads total (one per NodeKind) instead of 1,400. Each texture
 * paints a rounded card silhouette with the kind's accent color as
 * the header strip and a low-alpha body tint beneath.
 */
// Placeholder texture dims and nine-slice borders. The header strip
// height is encoded as `topHeight` so NineSliceSprite renders it at a
// fixed 32px regardless of how tall the node is — without nine-slice,
// stretching a 256×128 texture to fill a 600px-tall node would scale
// the header to ~150px, which looks wrong.
const PLACEHOLDER_TEX_W = 256;
const PLACEHOLDER_TEX_H = 128;
const PLACEHOLDER_HEADER_H = 32;
const PLACEHOLDER_CORNER = 11;

function buildKindPlaceholderTexture(kind: NodeKind): Texture {
  const w = PLACEHOLDER_TEX_W;
  const h = PLACEHOLDER_TEX_H;
  const headerH = PLACEHOLDER_HEADER_H;
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) return Texture.WHITE;
  const color = KIND_COLORS[kind];

  // Body: low-alpha kind color so the card reads as "muted" behind the
  // header. The card stays legible on both light and dark backgrounds
  // because Pixi composites against the scene's actual background.
  roundRect(ctx, 0, 0, w, h, 10);
  ctx.fillStyle = color;
  ctx.globalAlpha = 0.18;
  ctx.fill();

  // Header: full-opacity kind color strip across the top.
  ctx.globalAlpha = 1;
  roundRectTopOnly(ctx, 0, 0, w, headerH, 10);
  ctx.fillStyle = color;
  ctx.fill();

  // Outline to sharpen the card edge after scaling.
  ctx.strokeStyle = color;
  ctx.globalAlpha = 0.55;
  ctx.lineWidth = 1.5;
  roundRect(ctx, 0.75, 0.75, w - 1.5, h - 1.5, 9.5);
  ctx.stroke();
  ctx.globalAlpha = 1;

  return Texture.from(canvas);
}

// ─── Dashed bezier walker ─────────────────────────────────────────────

/** Sample a cubic bezier at parameter t */
function cubicBezier(
  p0: Point, p1: Point, p2: Point, p3: Point, t: number,
): Point {
  const mt = 1 - t;
  const mt2 = mt * mt;
  const t2 = t * t;
  return {
    x: mt2 * mt * p0.x + 3 * mt2 * t * p1.x + 3 * mt * t2 * p2.x + t2 * t * p3.x,
    y: mt2 * mt * p0.y + 3 * mt2 * t * p1.y + 3 * mt * t2 * p2.y + t2 * t * p3.y,
  };
}

// Flattened-polyline cache for edge hover hit-testing. Keyed weakly by
// the LaidEdge object so entries are collected when a layout replaces
// the edge list.
const edgePolylineCache = new WeakMap<LaidEdge, Float64Array>();
const EDGE_SAMPLE_STEPS = 16;

function edgePolyline(edge: LaidEdge): Float64Array {
  let pts = edgePolylineCache.get(edge);
  if (pts) return pts;
  const out: number[] = [edge.start.x, edge.start.y];
  let prev = edge.start;
  for (const seg of edge.segments) {
    for (let i = 1; i <= EDGE_SAMPLE_STEPS; i++) {
      const pt = cubicBezier(prev, seg.c1, seg.c2, seg.end, i / EDGE_SAMPLE_STEPS);
      out.push(pt.x, pt.y);
    }
    prev = seg.end;
  }
  pts = new Float64Array(out);
  edgePolylineCache.set(edge, pts);
  return pts;
}

const DIM_ALPHA = 0.1;
const STROKE_W = 1.4;

// Static edge-group definitions. Groups that were previously dashed
// get a reduced alphaScale (~0.55) so they read as "softer" /
// secondary against the solid groups — visually mirrors the old
// dashed-vs-solid contrast without per-dash vertex spam in Pixi.
interface EdgeGroupDef {
  color: string;
  colorHex: number;
  alphaScale: number;
}
const EDGE_GROUP_DEFS: EdgeGroupDef[] = [
  { color: "#3b82f6", colorHex: 0x3b82f6, alphaScale: 1 },    // [0] non-null field — solid blue
  { color: "#3b82f6", colorHex: 0x3b82f6, alphaScale: 0.45 }, // [1] nullable field — soft blue
  { color: "#eab308", colorHex: 0xeab308, alphaScale: 1 },    // [2] union member — solid amber
  { color: "#8b5cf6", colorHex: 0x8b5cf6, alphaScale: 0.55 }, // [3] implements — soft violet
  { color: "#f97316", colorHex: 0xf97316, alphaScale: 0.55 }, // [4] arg — soft orange
];

function edgeGroupIndex(e: LaidEdge): number {
  if (e.kind === "implements") return 3;
  if (e.kind === "union") return 2;
  if (e.kind === "arg") return 4;
  if (e.kind === "field" && e.nullable) return 1;
  return 0;
}

// ─── Feathered edge mesh ──────────────────────────────────────────────
//
// Edges are rendered as triangle-strip ribbons with analytic anti-
// aliasing in the fragment shader (screen-space smoothstep across the
// stroke), instead of Pixi Graphics strokes + framebuffer MSAA. The
// feather is computed from the live zoom uniform, so one static
// world-space geometry stays crisp at every zoom level; sub-pixel
// strokes fade out via coverage compensation instead of shimmering.
// The ribbon carries EDGE_FEATHER_PAD world-units of transparent
// margin so the ~1 screen-px feather has geometry to land on for any
// zoom where edges are legible.
const EDGE_FEATHER_PAD = 2.0;

let _edgeShader: Shader | null = null;
let _edgeUniforms: UniformGroup | null = null;

function getEdgeShader(): Shader {
  if (_edgeShader) return _edgeShader;
  _edgeUniforms = new UniformGroup({
    uZoom: { value: 1, type: "f32" },
  });
  _edgeShader = Shader.from({
    gl: {
      vertex: `
        attribute vec2 aPosition;
        attribute float aDist;
        attribute float aHalfW;
        attribute vec4 aColor;
        varying float vDist;
        varying float vHalfW;
        varying vec4 vColor;
        uniform mat3 uProjectionMatrix;
        uniform mat3 uWorldTransformMatrix;
        uniform mat3 uTransformMatrix;
        void main() {
          mat3 mvp = uProjectionMatrix * uWorldTransformMatrix * uTransformMatrix;
          gl_Position = vec4((mvp * vec3(aPosition, 1.0)).xy, 0.0, 1.0);
          vDist = aDist;
          vHalfW = aHalfW;
          vColor = aColor;
        }
      `,
      fragment: `
        precision mediump float;
        varying float vDist;
        varying float vHalfW;
        varying vec4 vColor;
        uniform vec4 uColor;
        uniform float uZoom;
        void main() {
          float dScreen = abs(vDist) * uZoom;
          float halfPx = max(0.5, vHalfW * uZoom);
          float alpha = 1.0 - smoothstep(halfPx - 0.75, halfPx + 0.75, dScreen);
          // Sub-pixel strokes: clamp rendered width to ~1px above and
          // scale alpha down by true coverage so thin lines dim out
          // smoothly instead of aliasing.
          alpha *= min(1.0, (vHalfW * uZoom) / 0.5);
          float a = vColor.a * alpha;
          // uColor is the premultiplied world color+alpha from the
          // mesh pipe — this is what makes container.alpha dimming
          // (focus mode) apply to the custom shader output.
          gl_FragColor = vec4(vColor.rgb * a, a) * uColor;
        }
      `,
    },
    resources: { edgeUniforms: _edgeUniforms },
  });
  return _edgeShader;
}

/** Ticker hook: keeps the shader's screen-space feather in sync with
 *  the current zoom. Cheap no-op until the first batch is built. */
function updateEdgeZoom(k: number) {
  if (!_edgeUniforms) return;
  (_edgeUniforms.uniforms as { uZoom: number }).uZoom = k;
  _edgeUniforms.update();
}

// Shared arrowhead texture: a white right-pointing triangle drawn once
// at 64px with canvas AA (plus mipmaps for minification), then tinted
// per sprite. Replaces per-arrow Graphics fills so arrowheads stay
// smooth without framebuffer MSAA and batch through Pixi's sprite
// batcher. Geometry: tip at x=60, base at x=4, half-height 22.4 —
// matching the old drawArrowHead proportions (length 7, half 2.8) at
// 8 texels per world unit.
const ARROW_TEX_SIZE = 64;
const ARROW_TIP_X = 60;
const ARROW_BASE_X = 4;
const ARROW_HALF_H = 22.4;
const ARROW_WORLD_LEN = 7;
// Sprite size in world units so the 64px texture maps 1:1 onto the
// old arrow proportions.
const ARROW_SPRITE_W = (ARROW_TEX_SIZE / (ARROW_TIP_X - ARROW_BASE_X)) * ARROW_WORLD_LEN;

let _arrowTexture: Texture | null = null;

function getArrowTexture(): Texture {
  if (_arrowTexture) return _arrowTexture;
  const canvas = document.createElement("canvas");
  canvas.width = ARROW_TEX_SIZE;
  canvas.height = ARROW_TEX_SIZE;
  const ctx = canvas.getContext("2d");
  if (!ctx) return Texture.WHITE;
  const cy = ARROW_TEX_SIZE / 2;
  ctx.fillStyle = "#ffffff";
  ctx.beginPath();
  ctx.moveTo(ARROW_TIP_X, cy);
  ctx.lineTo(ARROW_BASE_X, cy - ARROW_HALF_H);
  ctx.lineTo(ARROW_BASE_X, cy + ARROW_HALF_H);
  ctx.closePath();
  ctx.fill();
  const tex = Texture.from(canvas);
  tex.source.autoGenerateMipmaps = true;
  _arrowTexture = tex;
  return tex;
}

/** Tip position + direction angle for an edge's arrowhead, or null
 *  when the final segment is degenerate. */
function arrowPose(edge: LaidEdge): { x: number; y: number; angle: number } | null {
  const lastSeg = edge.segments[edge.segments.length - 1]!;
  const tangentFrom = edge.arrowTip ? lastSeg.end : lastSeg.c2;
  const tangentTo = edge.arrowTip ?? lastSeg.end;
  const adx = tangentTo.x - tangentFrom.x;
  const ady = tangentTo.y - tangentFrom.y;
  if (adx * adx + ady * ady <= 0) return null;
  return { x: tangentTo.x, y: tangentTo.y, angle: Math.atan2(ady, adx) };
}

/**
 * Build a single batch for a slice of edges belonging to one group
 * (same color + alphaScale): one feathered ribbon Mesh for the edge
 * bodies plus one Graphics for the arrowhead fills. Bounded to
 * EDGES_PER_BATCH edges so no single GPU upload is ever huge.
 */
function buildEdgeBatchMesh(
  slice: LaidEdge[],
  group: { colorHex: number; alphaScale: number },
  alpha: number,
  width: number = STROKE_W,
): Container {
  const batch = new Container();
  const halfW = width / 2;
  const hw = halfW + EDGE_FEATHER_PAD;
  const cr = ((group.colorHex >> 16) & 0xff) / 255;
  const cg = ((group.colorHex >> 8) & 0xff) / 255;
  const cb = (group.colorHex & 0xff) / 255;

  let vcount = 0;
  let icount = 0;
  const polys: (Float64Array | null)[] = [];
  for (const e of slice) {
    const pts = edgePolyline(e);
    const n = pts.length / 2;
    if (n < 2) {
      polys.push(null);
      continue;
    }
    polys.push(pts);
    vcount += 2 * n;
    icount += 6 * (n - 1);
  }

  if (vcount > 0) {
    const positions = new Float32Array(vcount * 2);
    const dists = new Float32Array(vcount);
    const halfWs = new Float32Array(vcount).fill(halfW);
    const colors = new Float32Array(vcount * 4);
    const indices = new Uint32Array(icount);
    let vi = 0;
    let ii = 0;
    for (let si = 0; si < slice.length; si++) {
      const pts = polys[si];
      if (!pts) continue;
      const e = slice[si]!;
      const n = pts.length / 2;
      const ea =
        alpha * group.alphaScale * ((e.hubFade ?? 1) < 1 ? HUB_FADE_ALPHA : 1);
      const base = vi;
      for (let i = 0; i < n; i++) {
        const x = pts[2 * i]!;
        const y = pts[2 * i + 1]!;
        // Averaged tangent of the two adjacent segments — polylines
        // sampled from beziers are smooth, so no miter correction
        // is needed.
        const iPrev = Math.max(0, i - 1);
        const iNext = Math.min(n - 1, i + 1);
        let dx = pts[2 * iNext]! - pts[2 * iPrev]!;
        let dy = pts[2 * iNext + 1]! - pts[2 * iPrev + 1]!;
        const len = Math.hypot(dx, dy) || 1;
        dx /= len;
        dy /= len;
        const nx = -dy;
        const ny = dx;
        for (let side = 0; side < 2; side++) {
          const sgn = side === 0 ? 1 : -1;
          positions[vi * 2] = x + nx * hw * sgn;
          positions[vi * 2 + 1] = y + ny * hw * sgn;
          dists[vi] = hw * sgn;
          colors[vi * 4] = cr;
          colors[vi * 4 + 1] = cg;
          colors[vi * 4 + 2] = cb;
          colors[vi * 4 + 3] = ea;
          vi++;
        }
      }
      for (let i = 0; i < n - 1; i++) {
        const v0 = base + 2 * i;
        indices[ii++] = v0;
        indices[ii++] = v0 + 1;
        indices[ii++] = v0 + 2;
        indices[ii++] = v0 + 1;
        indices[ii++] = v0 + 3;
        indices[ii++] = v0 + 2;
      }
    }
    const geometry = new Geometry({
      attributes: {
        aPosition: { buffer: positions, format: "float32x2" },
        aDist: { buffer: dists, format: "float32" },
        aHalfW: { buffer: halfWs, format: "float32" },
        aColor: { buffer: colors, format: "float32x4" },
      },
      indexBuffer: indices,
    });
    const mesh = new Mesh({ geometry, shader: getEdgeShader() });
    // The shader is shared across every batch; the geometry is not —
    // make sure its GPU buffers go away with the mesh (Mesh.destroy
    // does not own the geometry).
    mesh.once("destroyed", () => geometry.destroy());
    batch.addChild(mesh);
  }

  // Arrowheads: tinted sprites sharing one feathered triangle texture
  // — smooth without MSAA and coalesced by Pixi's sprite batcher.
  const arrowTex = getArrowTexture();
  const effAlpha = alpha * group.alphaScale;
  for (const e of slice) {
    const pose = arrowPose(e);
    if (!pose) continue;
    const spr = new Sprite(arrowTex);
    spr.anchor.set(ARROW_TIP_X / ARROW_TEX_SIZE, 0.5);
    spr.width = ARROW_SPRITE_W;
    spr.height = ARROW_SPRITE_W;
    spr.position.set(pose.x, pose.y);
    spr.rotation = pose.angle;
    spr.tint = group.colorHex;
    spr.alpha = effAlpha * ((e.hubFade ?? 1) < 1 ? HUB_FADE_ALPHA : 1);
    batch.addChild(spr);
  }
  return batch;
}

/**
 * Total number of batches the tile will produce when fully built.
 * Each group contributes ⌈edges/N⌉ batches.
 */
function plannedBatchCount(tile: EdgeTile): number {
  let total = 0;
  for (const list of tile.groupLists) {
    total += Math.ceil(list.length / EDGES_PER_BATCH);
  }
  return total;
}

/**
 * Build the `batchIdx`-th batch of the tile. Returns null when the
 * index is past the tile's planned batches (defensive — the caller
 * should already be gating on `totalBatches`). Tiles are always built
 * at full (non-dim) alpha; focus dimming is applied wholesale via the
 * tile containers' alpha so it never invalidates tile geometry.
 */
function buildEdgeTileBatch(
  tile: EdgeTile,
  batchIdx: number,
): Container | null {
  let idx = batchIdx;
  for (let gi = 0; gi < EDGE_GROUP_DEFS.length; gi++) {
    const list = tile.groupLists[gi];
    if (!list) continue;
    const batches = Math.ceil(list.length / EDGES_PER_BATCH);
    if (idx < batches) {
      const start = idx * EDGES_PER_BATCH;
      const end = Math.min(start + EDGES_PER_BATCH, list.length);
      return buildEdgeBatchMesh(list.slice(start, end), EDGE_GROUP_DEFS[gi]!, 1);
    }
    idx -= batches;
  }
  return null;
}



// ─── Dot grid tile builder ────────────────────────────────────────────

function buildDotGridTexture(dotColor: number, alpha: number): Texture {
  const size = 24;
  const canvas = document.createElement("canvas");
  canvas.width = size;
  canvas.height = size;
  const ctx = canvas.getContext("2d")!;
  const r = (dotColor >> 16) & 0xff;
  const gv = (dotColor >> 8) & 0xff;
  const b = dotColor & 0xff;
  ctx.fillStyle = `rgba(${r},${gv},${b},${alpha})`;
  ctx.fillRect(0, 0, 1, 1);
  return Texture.from(canvas);
}

// ─── Main component ───────────────────────────────────────────────────

export function SchemaCanvas({ nodes, edges, focusId, rootId, onNavigate, onClearFocus, leftControlsInset }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const pixiContainerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 1, h: 1 });
  const viewRef = useRef({ x: 0, y: 0, k: 1 });
  // Restarts the (possibly idle-stopped) Pixi ticker. Assigned once
  // the Application is initialized; safe to call before that (no-op).
  const wakeRef = useRef<() => void>(() => {});

  const dragRef = useRef({
    active: false,
    lastX: 0,
    lastY: 0,
    startX: 0,
    startY: 0,
    moved: false,
  });

  const hoveredFieldRef = useRef<{
    nodeId: string;
    fieldIndex: number;
    isRelayHover: boolean;
    isReturnTypeHover: boolean;
    returnTypeRect: { x: number; y: number; w: number; h: number } | null;
  } | null>(null);
  const hoveredNodeRef = useRef<string | null>(null);
  const hoveredEdgeRef = useRef<LaidEdge | null>(null);
  // React state mirror — only updates on hover-change so we don't
  // re-render on every mouse move. The label string drives both the
  // tooltip render and the redraw of the highlight Graphics.
  const [hoveredEdgeInfo, setHoveredEdgeInfo] = useState<{
    label: string;
    sourceId: string;
    targetId: string;
    kind: GraphEdgeData["kind"];
  } | null>(null);
  // Edge selected by click — dims everything except this edge and
  // its two endpoint nodes. Mutually exclusive with node focus
  // (clicking a node clears this, and vice versa).
  const [focusedEdge, setFocusedEdge] = useState<LaidEdge | null>(null);

  // Click history — last 50 entries, newest first. Surfaces a quick
  // "recently visited" jump list overlaid on the canvas.
  type HistoryItem =
    | { kind: "node"; id: string; nodeId: string; name: string; nodeKind: NodeKind; ts: number }
    | {
        kind: "field";
        id: string;
        typeId: string;
        typeName: string;
        fieldName: string;
        fieldIndex: number;
        nodeKind: NodeKind;
        ts: number;
      }
    | {
        kind: "edge";
        id: string;
        sourceId: string;
        targetId: string;
        label: string;
        edgeKind: GraphEdgeData["kind"];
        ts: number;
      };
  const HISTORY_CAP = 50;
  const [clickHistory, setClickHistory] = useState<HistoryItem[]>([]);
  const [historyOpen, setHistoryOpen] = useState(true);
  const [hoveredHistoryItem, setHoveredHistoryItem] = useState<HistoryItem | null>(null);
  // Tooltip positions live in refs (not state) — a mousemove over a
  // node/edge/history row repositions the already-mounted tooltip DOM
  // node directly instead of re-rendering this whole component per
  // pointer event. Only the tooltip *content* (which changes rarely,
  // on hover-target change) goes through React state.
  const hoveredHistoryPosRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const historyTipElRef = useRef<HTMLDivElement | null>(null);
  const moveHistoryTip = (x: number, y: number) => {
    hoveredHistoryPosRef.current = { x, y };
    if (historyTipElRef.current) applyTooltipStyle(historyTipElRef.current, x, y);
  };

  // Investigate mode — when active, only items matching the
  // predicate render at full opacity in orange; everything else
  // fades to a muted gray. Designed to be extended with more checks
  // (e.g. unused types, deprecation, complexity) — `description`
  // is the first one and highlights nodes whose own description or
  // any of whose fields/values lack a description.
  type InvestigateMode = "off" | "description";
  const [investigateMode, setInvestigateMode] = useState<InvestigateMode>("off");

  // Description coverage % across every documentable item — nodes,
  // their fields, and enum values. Shown next to the "Missing
  // descriptions" toggle so the user knows at a glance how well the
  // schema is documented.
  const descriptionCoverage = useMemo(() => {
    let total = 0;
    let documented = 0;
    for (const n of nodes) {
      total += 1;
      if (n.description?.trim()) documented += 1;
      for (const f of n.fields ?? []) {
        total += 1;
        // A deprecated field with a deprecationReason is treated as
        // documented — the reason explains the field's purpose for
        // coverage purposes.
        if (f.description?.trim() || (f.isDeprecated && f.deprecationReason?.trim())) {
          documented += 1;
        }
      }
      for (const v of n.values ?? []) {
        total += 1;
        if (v.description?.trim() || (v.isDeprecated && v.deprecationReason?.trim())) {
          documented += 1;
        }
      }
    }
    return total === 0 ? 1 : documented / total;
  }, [nodes]);
  const pushHistory = (item: HistoryItem) => {
    setClickHistory((prev) => {
      // De-dupe by id against the most-recent entry so spamming the
      // same row doesn't fill the list with duplicates.
      if (prev[0]?.id === item.id) return prev;
      return [item, ...prev.filter((p) => p.id !== item.id)].slice(0, HISTORY_CAP);
    });
  };
  const removeFromHistory = (id: string) => {
    setClickHistory((prev) => prev.filter((p) => p.id !== id));
  };

  /**
   * Shared "focus this edge" action used by both the in-canvas edge
   * click and the history-item click. Frames the two endpoint nodes
   * to fit, sets focus state (which triggers dimming of everything
   * else + full-LOD render of the endpoints), clears any in-flight
   * hover, and bumps the entry to the top of the history.
   */
  const focusOnEdge = (edge: LaidEdge) => {
    const a = nodeById.get(edge.sourceId);
    const b = nodeById.get(edge.targetId);
    if (a && b) {
      const minX = Math.min(a.cx - a.w / 2, b.cx - b.w / 2);
      const maxX = Math.max(a.cx + a.w / 2, b.cx + b.w / 2);
      const minY = Math.min(a.cy - a.h / 2, b.cy - b.h / 2);
      const maxY = Math.max(a.cy + a.h / 2, b.cy + b.h / 2);
      const pad = 80;
      const gW = maxX - minX + pad * 2;
      const gH = maxY - minY + pad * 2;
      const k = Math.max(0.1, Math.min(size.w / gW, size.h / gH, 1.2));
      const cx = (minX + maxX) / 2;
      const cy = (minY + maxY) / 2;
      viewRef.current = {
        k,
        x: size.w / 2 - cx * k,
        y: size.h / 2 - cy * k,
      };
    }
    hoveredEdgeRef.current = null;
    setHoveredEdgeInfo(null);
    setFocusedEdge(edge);
    pushHistory({
      kind: "edge",
      id: `edge:${edge.sourceId}|${edge.targetId}|${edge.label ?? ""}|${edge.kind}`,
      sourceId: edge.sourceId,
      targetId: edge.targetId,
      label: edge.label ?? "",
      edgeKind: edge.kind,
      ts: Date.now(),
    });
  };
  const hoveredEdgeScreenRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const edgeTipElRef = useRef<HTMLDivElement | null>(null);
  // Node-name tooltip — only rendered at low LODs (bar / chrome)
  // where the sprite no longer paints the type name.
  const hoveredNodeForTipRef = useRef<string | null>(null);
  const [hoveredNodeTip, setHoveredNodeTip] = useState<{
    name: string;
    kind: NodeKind;
  } | null>(null);
  const hoveredNodeScreenRef = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const nodeTipElRef = useRef<HTMLDivElement | null>(null);
  const [cursor, setCursor] = useState<"grab" | "pointer">("grab");
  const { resolved: themeResolved } = useTheme();
  const {
    hidePrimitiveFields,
    setHidePrimitiveFields,
    hideRelayBoilerplate,
    setHideRelayBoilerplate,
    pinnedField,
    setPinnedField,
  } = useSchema();
  // "Show descriptions" toggle. When on, every node card reserves
  // an extra italic line under each field name and an extra block
  // under the type name in the header so the SDL descriptions
  // render inline. Layout has to re-run on toggle (node heights
  // change) — this is tracked via the layout effect's deps below.
  const [showGraphDescriptions, setShowGraphDescriptions] = useState(true);
  const currentLodRef = useRef<SpriteLOD>("full");
  const [lodTick, setLodTick] = useState(0);
  const [appReady, setAppReady] = useState(false);

  // Layout state
  const [layoutResult, setLayoutResult] = useState<LayoutResult>(EMPTY_LAYOUT);
  // The `nodes` array `layoutResult` was computed for. When the mode/tab
  // switches, `nodes` changes immediately but the async layout hasn't caught
  // up — this lets `laidNodes` detect the mismatch and keep the previous
  // graph instead of flashing an empty canvas.
  const layoutForNodesRef = useRef<GraphNodeData[] | null>(null);
  const prevLaidNodesRef = useRef<LaidNode[]>([]);
  const [isPending, setIsPending] = useState(nodes.length > 0);
  const [layoutProgress, setLayoutProgress] = useState<{ done: number; total: number } | null>(null);
  const [lastTiming, setLastTiming] = useState<OrchestratorTimings | null>(null);
  const lastTimingRef = useRef<OrchestratorTimings | null>(null);
  const orchestratorRef = useRef<LayoutOrchestrator | null>(null);
  const requestIdRef = useRef(0);

  // Pixi app + scene graph refs
  const appRef = useRef<Application | null>(null);
  const sceneRef = useRef<{
    gridTiling: TilingSprite | null;
    world: Container | null;
    edgeTileContainer: Container | null;
    activeEdgeContainer: Container | null;
    focusEdgeGraphics: Container | null;
    hoverEdgeGraphics: Container | null;
    nodeContainer: Container | null;
    /** SDF text meshes, one per full-LOD node, above the card sprites. */
    textContainer: Container | null;
    investigateOverlay: Graphics | null;
    pinFieldGraphics: Graphics | null;
    hoverGraphics: Graphics | null;
    focusGraphics: Graphics | null;
  }>({
    gridTiling: null,
    world: null,
    edgeTileContainer: null,
    activeEdgeContainer: null,
    focusEdgeGraphics: null,
    hoverEdgeGraphics: null,
    nodeContainer: null,
    textContainer: null,
    investigateOverlay: null,
    pinFieldGraphics: null,
    hoverGraphics: null,
    focusGraphics: null,
  });

  // Node sprite/texture cache
  const textureCacheRef = useRef(new Map<string, Texture>());
  const spriteDprRef = useRef(0);
  const nodeSpritesRef = useRef(new Map<string, NineSliceSprite>());
  const spriteCtxRef = useRef<SpriteCtx | null>(null);
  // SDF text meshes, keyed by node id. Lifecycle is tied 1:1 to the
  // node's full-LOD texture cache entry — created when that texture is
  // built, destroyed whenever it is purged/evicted.
  const nodeTextMeshesRef = useRef(new Map<string, Container>());

  // Stable helpers for the ticker closure (touch only refs).
  const ensureNodeTextMeshRef = useRef((node: LaidNode, runs: TextRun[]) => {
    const textContainer = sceneRef.current.textContainer;
    if (!textContainer) return;
    if (nodeTextMeshesRef.current.has(node.id)) return;
    const mesh = buildNodeTextMesh(runs, node.w, node.h);
    if (!mesh) return;
    mesh.position.set(node.cx - node.w / 2, node.cy - node.h / 2);
    if (edgeGroupsRef.current.dimNodeIds.has(node.id)) mesh.alpha = 0.1;
    textContainer.addChild(mesh);
    nodeTextMeshesRef.current.set(node.id, mesh);
  });
  const destroyNodeTextMeshRef = useRef((id: string) => {
    const tm = nodeTextMeshesRef.current.get(id);
    if (!tm) return;
    tm.parent?.removeChild(tm);
    tm.destroy({ children: true });
    nodeTextMeshesRef.current.delete(id);
  });

  // Edge tile cache — spatial grid of per-tile Graphics. See `TILE_SIZE`
  // below. Each tile is built lazily when it first enters the viewport
  // and destroyed after `TILE_EVICT_FRAMES` frames off-screen, capping
  // GPU memory so large schemas don't crash the mobile renderer.
  const edgeTilesRef = useRef(new Map<string, EdgeTile>());
  const frameCounterRef = useRef(0);

  // Per-sprite viewport bookkeeping. `spriteLastSeenFrameRef` tracks
  // the last frame a sprite was inside the (padded) viewport so the
  // ticker can destroy textures for sprites that have been off-screen
  // for long enough — this is what lets us crank DPR up on the "full"
  // tier without holding textures for all N nodes at once.
  const spriteLastSeenFrameRef = useRef(new Map<string, number>());
  const lastSpriteSweepViewRef = useRef({ x: 0, y: 0, k: 0, lod: "full" as SpriteLOD });
  // Progressive sprite-creation queue. On a big schema the viewport
  // sweep can discover thousands of nodes needing a Sprite at once
  // (e.g. zooming out to see the whole graph). Allocating that many
  // `new Sprite` + `addChild` pairs in a single frame overwhelms
  // the Pixi renderer hard enough to crash the tab, so we defer to
  // this queue and drain a budgeted chunk per frame.
  const spriteCreateQueueRef = useRef<LaidNode[] | null>(null);

  // Shared "kind placeholder" textures — one per node kind. Used as
  // sprite source at chrome/bar LOD instead of Texture.WHITE + tint.
  // Lets us paint rounded corners and a header strip (proper card
  // silhouette) without paying for 1,400 individual texture uploads.
  const kindTextureCacheRef = useRef<Map<NodeKind, Texture>>(new Map());
  // Last timestamp the view changed significantly. Texture uploads
  // (Pixi `Texture.from(canvas)` → WebGL `texImage2D`) and tile
  // Graphics builds are gated on this: during an active pan/zoom we
  // pause all GPU uploads so the mobile driver doesn't get flooded
  // and crash the renderer. Once the view has been stable for
  // `MOTION_SETTLE_MS` we resume building progressively.
  const lastViewChangeAtRef = useRef(0);
  // Set to true when the view jumps in one go because of an explicit
  // focus change (tree-panel or canvas click → navigation). For one
  // frame after the jump the ticker bypasses the motion-settle gate
  // and drains the texture build queue with a larger budget so the
  // user doesn't sit on chrome/bar placeholders for ~150 ms. Cleared
  // by the drain loop once the queue is empty.
  const focusJumpPendingRef = useRef(false);

  // Progressive sprite build queue — filled by the node useEffect,
  // drained by the ticker a few nodes per frame (budget-limited).
  interface SpriteBuildQueue {
    nodes: LaidNode[];
    lod: SpriteLOD;
    dpr: number;
    spriteCtx: SpriteCtx;
    dimNodeIds: Set<string>;
  }
  const spriteBuildQueueRef = useRef<SpriteBuildQueue | null>(null);

  // FPS overlay — written straight to the DOM from the ticker (like
  // the chart canvas) so the 200 ms sampling doesn't re-render this
  // whole component 5×/sec forever.
  const fpsTextRef = useRef<HTMLSpanElement>(null);
  const fpsHistoryRef = useRef<number[]>(new Array(60).fill(0));
  const chartCanvasRef = useRef<HTMLCanvasElement>(null);

  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const r = entries[0]?.contentRect;
      if (r) setSize({ w: Math.max(1, r.width), h: Math.max(1, r.height) });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Pool of layout workers. Orchestrator splits the graph into
  // weakly-connected components and dispatches each to a free worker
  // in parallel, so large schemas finish in wall-clock ~= (biggest
  // component time) rather than (sum of all components).
  useEffect(() => {
    const orch = new LayoutOrchestrator(defaultPoolSize());
    orch.setFatalHandler((err) => {
      console.error("layout orchestrator error:", err.message);
      setIsPending(false);
    });
    orchestratorRef.current = orch;
    return () => {
      orch.terminate();
      orchestratorRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (nodes.length === 0) {
      requestIdRef.current += 1;
      layoutForNodesRef.current = nodes;
      prevLaidNodesRef.current = [];
      setLayoutResult(EMPTY_LAYOUT);
      setIsPending(false);
      return;
    }
    const orch = orchestratorRef.current;
    if (!orch) return;
    const layoutNodes = nodes.map((n) => {
      const interfaceRows: [string, string][] = (n.interfaces ?? []).map(
        (iface): [string, string] => [iface, ""],
      );
      const unionRows: [string, string][] = (n.memberOfUnions ?? []).map(
        (u): [string, string] => [u, ""],
      );
      const bodyRows: [string, string][] =
        n.kind === "Enum"
          ? (n.values?.map((v): [string, string] => [v.name, ""]) ?? [])
          : n.kind === "Union"
            ? (n.members?.map((m): [string, string] => [m, ""]) ?? [])
            : [
                // Width must fit the *rendered* type string (x.type —
                // includes wrappers, and for unwrapped Relay fields the
                // original Connection name, which x.typeName no longer
                // carries). Relay rows lead with a stand-in for the ~
                // icon's width.
                ...(n.fields?.map((x): [string, string] => [
                  x.name,
                  (x.isRelayConnection ? "~~ " : "") + x.type,
                ]) ?? []),
                ...interfaceRows,
                ...unionRows,
              ];
      return {
        id: n.id,
        width: estimateNodeWidth(n.name, bodyRows),
        height: estimateNodeHeight(
          n.kind,
          n.fields?.length ?? 0,
          n.values?.length ?? 0,
          n.members?.length ?? 0,
          n.interfaces?.length ?? 0,
          showGraphDescriptions,
          n.memberOfUnions?.length ?? 0,
        ),
      };
    });
    const id = ++requestIdRef.current;
    setIsPending(true);
    setLayoutProgress(null);
    const request: OrchestratorRequest = {
      id,
      nodes,
      edges,
      layoutNodes,
      rootId: rootId ?? null,
      onProgress: (done, total) => {
        // Stale-request guard: a newer request may have been issued
        // while this one was mid-flight.
        if (id !== requestIdRef.current) return;
        setLayoutProgress({ done, total });
      },
    };
    orch
      .layout(request)
      .then((resp) => {
        if (resp.id !== requestIdRef.current) return;
        layoutForNodesRef.current = nodes;
        setLayoutResult(resp.result);
        setLastTiming(resp.timings);
        lastTimingRef.current = resp.timings;
        setIsPending(false);
        setLayoutProgress(null);
      })
      .catch((err: Error) => {
        console.error("layout failed:", err.message);
        setIsPending(false);
        setLayoutProgress(null);
      });
  }, [nodes, edges, rootId, showGraphDescriptions]);

  const laidNodes = useMemo<LaidNode[]>(() => {
    // Stale guard: `layoutResult` was computed for a *different* `nodes`
    // array (a mode/tab switch just changed the node set and the new
    // layout is still in flight). Building now would filter the old
    // positions down to the intersection — empty for disjoint sets — and
    // flash a blank canvas. Keep the last good graph until the matching
    // layout lands.
    if (layoutForNodesRef.current !== nodes && prevLaidNodesRef.current.length > 0) {
      return prevLaidNodesRef.current;
    }
    const byId = new Map<string, GraphNodeData>();
    for (const n of nodes) byId.set(n.id, n);
    const rowH = rowHFor(showGraphDescriptions);
    const headerH = headerHFor(showGraphDescriptions);
    const built = layoutResult.nodes
      .filter((p) => byId.has(p.id))
      .map((p) => ({
        id: p.id,
        data: byId.get(p.id)!,
        cx: p.x,
        cy: p.y,
        w: p.width,
        h: p.height,
        rowH,
        headerH,
      }));
    prevLaidNodesRef.current = built;
    return built;
  }, [layoutResult, nodes, showGraphDescriptions]);

  const nodeById = useMemo(() => {
    const m = new Map<string, LaidNode>();
    for (const n of laidNodes) m.set(n.id, n);
    return m;
  }, [laidNodes]);

  // Spatial grid for node hit-testing — same TILE_SIZE cells as the
  // edge tiles. Nodes spanning multiple cells are registered in each,
  // so a point lookup only ever needs its own cell. Keeps the
  // per-mousemove hit tests O(nodes in cell) instead of O(all nodes).
  const nodeTileIndex = useMemo(() => {
    const m = new Map<string, LaidNode[]>();
    for (const n of laidNodes) {
      const minCol = Math.floor((n.cx - n.w / 2) / TILE_SIZE);
      const maxCol = Math.floor((n.cx + n.w / 2) / TILE_SIZE);
      const minRow = Math.floor((n.cy - n.h / 2) / TILE_SIZE);
      const maxRow = Math.floor((n.cy + n.h / 2) / TILE_SIZE);
      for (let c = minCol; c <= maxCol; c++) {
        for (let r = minRow; r <= maxRow; r++) {
          const key = `${c},${r}`;
          const list = m.get(key);
          if (list) list.push(n);
          else m.set(key, [n]);
        }
      }
    }
    return m;
  }, [laidNodes]);

  const nodesAt = (worldX: number, worldY: number): LaidNode[] =>
    nodeTileIndex.get(
      `${Math.floor(worldX / TILE_SIZE)},${Math.floor(worldY / TILE_SIZE)}`,
    ) ?? [];

  const laidEdges = useMemo<LaidEdge[]>(() => {
    const byEdgeId = new Map<string, (typeof layoutResult.edgePaths)[number]>();
    for (const p of layoutResult.edgePaths) byEdgeId.set(p.edgeId, p);
    const out: LaidEdge[] = [];
    for (const e of edges) {
      if (e.source === e.target) continue;
      const a = nodeById.get(e.source);
      const b = nodeById.get(e.target);
      if (!a || !b) continue;
      const path = byEdgeId.get(e.id);
      if (!path || path.segments.length === 0) continue;

      let start: Point = { x: path.start.x, y: path.start.y };
      let segments: BezierSegment[] = path.segments;
      const arrowTip = path.arrowTip;

      if (e.kind === "field" && e.sourceFieldIndex != null && b.cx > a.cx && segments.length > 0) {
        const exitX = a.cx + a.w / 2;
        const exitY = a.cy - a.h / 2 + a.headerH + TOP_BODY_PAD - 2 + e.sourceFieldIndex * a.rowH + 6;
        const origC1 = segments[0]!.c1;
        const tangentLen = Math.hypot(origC1.x - start.x, origC1.y - start.y);
        const c1Offset = Math.max(tangentLen, 32);
        start = { x: exitX, y: exitY };
        segments = [
          {
            c1: { x: exitX + c1Offset, y: exitY },
            c2: segments[0]!.c2,
            end: segments[0]!.end,
          },
          ...segments.slice(1),
        ];
      }

      let minX = start.x, maxX = start.x, minY = start.y, maxY = start.y;
      for (const s of segments) {
        if (s.c1.x < minX) minX = s.c1.x; else if (s.c1.x > maxX) maxX = s.c1.x;
        if (s.c1.y < minY) minY = s.c1.y; else if (s.c1.y > maxY) maxY = s.c1.y;
        if (s.c2.x < minX) minX = s.c2.x; else if (s.c2.x > maxX) maxX = s.c2.x;
        if (s.c2.y < minY) minY = s.c2.y; else if (s.c2.y > maxY) maxY = s.c2.y;
        if (s.end.x < minX) minX = s.end.x; else if (s.end.x > maxX) maxX = s.end.x;
        if (s.end.y < minY) minY = s.end.y; else if (s.end.y > maxY) maxY = s.end.y;
      }
      if (arrowTip) {
        if (arrowTip.x < minX) minX = arrowTip.x; else if (arrowTip.x > maxX) maxX = arrowTip.x;
        if (arrowTip.y < minY) minY = arrowTip.y; else if (arrowTip.y > maxY) maxY = arrowTip.y;
      }

      out.push({
        sourceId: e.source,
        targetId: e.target,
        kind: e.kind,
        nullable: e.nullable ?? false,
        label: e.label,
        start,
        segments,
        arrowTip,
        bbox: { minX, minY, maxX, maxY },
      });
    }

    // Hub detection: any node with ≥ HUB_FADE_DEGREE incoming OR
    // outgoing edges (counted on the rendered/laid-out edge set) is
    // a hub. Edges touching a hub get a reduced opacity multiplier
    // so the visual doesn't get dominated by hub fan-out / fan-in.
    const outDeg = new Map<string, number>();
    const inDeg = new Map<string, number>();
    for (const le of out) {
      outDeg.set(le.sourceId, (outDeg.get(le.sourceId) ?? 0) + 1);
      inDeg.set(le.targetId, (inDeg.get(le.targetId) ?? 0) + 1);
    }
    const hubIds = new Set<string>();
    for (const [id, d] of outDeg) if (d >= HUB_FADE_DEGREE) hubIds.add(id);
    for (const [id, d] of inDeg) if (d >= HUB_FADE_DEGREE) hubIds.add(id);
    if (hubIds.size > 0) {
      for (const le of out) {
        if (hubIds.has(le.sourceId) || hubIds.has(le.targetId)) {
          le.hubFade = HUB_FADE_ALPHA;
        }
      }
    }
    return out;
  }, [edges, layoutResult, nodeById]);

  /**
   * Investigate-mode predicate evaluation. Returns three sets:
   *   - nodeIds: nodes that match in any way (kept at full opacity)
   *   - nodeOutline: nodes whose OWN description is missing (gets a
   *     full-node orange outline)
   *   - rowsByNode: per-node set of row indices whose specific
   *     field/value description is missing (gets a thin orange row
   *     stripe so the user can see exactly which rows are missing)
   * Memoized on the raw node list because the predicate doesn't
   * depend on layout, only on schema content.
   */
  const investigateMatch = useMemo<
    | {
        nodeIds: Set<string>;
        nodeOutline: Set<string>;
        rowsByNode: Map<string, Set<number>>;
      }
    | null
  >(() => {
    if (investigateMode === "off") return null;
    const nodeIds = new Set<string>();
    const nodeOutline = new Set<string>();
    const rowsByNode = new Map<string, Set<number>>();
    if (investigateMode === "description") {
      for (const n of nodes) {
        let matched = false;
        if (!n.description?.trim()) {
          nodeOutline.add(n.id);
          matched = true;
        }
        const rows = new Set<number>();
        const fields = n.fields ?? [];
        for (let i = 0; i < fields.length; i++) {
          const f = fields[i]!;
          if (!f.description?.trim() && !(f.isDeprecated && f.deprecationReason?.trim())) {
            rows.add(i);
          }
        }
        const values = n.values ?? [];
        for (let i = 0; i < values.length; i++) {
          const v = values[i]!;
          if (!v.description?.trim() && !(v.isDeprecated && v.deprecationReason?.trim())) {
            rows.add(i);
          }
        }
        if (rows.size > 0) {
          rowsByNode.set(n.id, rows);
          matched = true;
        }
        if (matched) nodeIds.add(n.id);
      }
    }
    return { nodeIds, nodeOutline, rowsByNode };
  }, [investigateMode, nodes]);

  // Static per-group edge buckets — depends only on the laid-out edge
  // set, never on focus state. This is what the edge tiles are built
  // from, so navigating (focus changes) doesn't invalidate any tile
  // geometry.
  const edgeBuckets = useMemo<LaidEdge[][]>(() => {
    const buckets: LaidEdge[][] = EDGE_GROUP_DEFS.map(() => []);
    for (const e of laidEdges) buckets[edgeGroupIndex(e)]!.push(e);
    return buckets;
  }, [laidEdges]);

  const edgeGroups = useMemo((): EdgeGroups => {
    // Dim mode precedence: an explicit edge selection (click) takes
    // priority over the tree-panel node focus. Both keep the same
    // dim-vs-active partition; only the predicate differs.
    let activePred: ((e: LaidEdge) => boolean) | null = null;
    const dimNodeIds = new Set<string>();
    if (focusedEdge) {
      activePred = (e) => e === focusedEdge;
      const keep = new Set<string>([focusedEdge.sourceId, focusedEdge.targetId]);
      // Walk every laid-out node so isolated singletons get dimmed
      // too, not just nodes that happen to participate in some edge.
      for (const n of laidNodes) {
        if (!keep.has(n.id)) dimNodeIds.add(n.id);
      }
    } else if (focusId && focusId !== rootId) {
      activePred = (e) => e.sourceId === focusId || e.targetId === focusId;
      const connectedIds = new Set<string>([focusId]);
      for (const e of laidEdges) {
        if (e.sourceId === focusId) connectedIds.add(e.targetId);
        else if (e.targetId === focusId) connectedIds.add(e.sourceId);
      }
      for (const e of laidEdges) {
        if (!connectedIds.has(e.sourceId)) dimNodeIds.add(e.sourceId);
        if (!connectedIds.has(e.targetId)) dimNodeIds.add(e.targetId);
      }
    }

    const groups: EdgeGroupSpec[] = edgeBuckets.map((edgeList, gi) => {
      const { color, colorHex, alphaScale } = EDGE_GROUP_DEFS[gi]!;
      if (!activePred) return { color, colorHex, alphaScale, dim: [], active: edgeList };
      const pred = activePred;
      return {
        color,
        colorHex,
        alphaScale,
        dim: edgeList.filter((e) => !pred(e)),
        active: edgeList.filter((e) => pred(e)),
      };
    });

    // Investigate mode overrides the active/dim partition for edges
    // — we don't highlight edges at all, so every edge moves into
    // the dim list. Node dimming stacks with any focus state so
    // non-matched nodes also fade out.
    if (investigateMatch) {
      for (const g of groups) {
        if (g.active.length > 0) {
          g.dim = g.dim.length === 0 ? g.active : g.dim.concat(g.active);
          g.active = [];
        }
      }
      for (const n of laidNodes) {
        if (!investigateMatch.nodeIds.has(n.id)) dimNodeIds.add(n.id);
      }
    }

    return { groups, dimNodeIds };
  }, [edgeBuckets, laidEdges, laidNodes, focusId, rootId, focusedEdge, investigateMatch]);

  // The focused-edge reference becomes stale when laidEdges rebuilds
  // (new layout / schema change). Clear it so the dim state doesn't
  // get stuck on an orphan object.
  useEffect(() => {
    if (!focusedEdge) return;
    if (!laidEdges.includes(focusedEdge)) setFocusedEdge(null);
  }, [laidEdges, focusedEdge]);

  // Ticker-accessible mirror of `focusedEdge`. The sprite sweep runs
  // outside React render and needs to force full-LOD rendering for
  // the focused edge's two endpoints regardless of the global zoom.
  const focusedEdgeRef = useRef<LaidEdge | null>(focusedEdge);
  focusedEdgeRef.current = focusedEdge;

  // Whenever the focused edge changes, invalidate the sprite sweep's
  // last-view cache so the next frame re-evaluates endpoints (which
  // need a synchronous full-LOD build) and other sprites (which can
  // drop back to the placeholder).
  useEffect(() => {
    lastSpriteSweepViewRef.current = { x: NaN, y: NaN, k: 0, lod: "full" };
  }, [focusedEdge]);

  const bounds = useMemo(() => {
    if (laidNodes.length === 0) return { minX: 0, minY: 0, maxX: 0, maxY: 0 };
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const n of laidNodes) {
      const x1 = n.cx - n.w / 2, y1 = n.cy - n.h / 2;
      const x2 = n.cx + n.w / 2, y2 = n.cy + n.h / 2;
      if (x1 < minX) minX = x1;
      if (y1 < minY) minY = y1;
      if (x2 > maxX) maxX = x2;
      if (y2 > maxY) maxY = y2;
    }
    return { minX, minY, maxX, maxY };
  }, [laidNodes]);

  // Auto-fit + focus pan (merged into one effect to reduce effect count).
  // Auto-fit runs once per unique (nodeCount, viewport size) to center the
  // whole graph. Focus pan runs on explicit type selection in the tree.
  //
  // useLayoutEffect (not useEffect): `laidNodesRef` is updated during render,
  // so the Pixi ticker (an rAF loop) can paint a frame using the freshly-laid
  // nodes before a *passive* effect would run. With useEffect the first such
  // frame renders at the initial view (k=1) and the initial LOD ("full") —
  // a flash of giant full-detail cards at the origin before the fit lands.
  // A layout effect runs synchronously after commit, before paint and before
  // the next rAF, so viewRef + currentLodRef are correct on the first frame.
  // Tracks the `laidNodes` reference we last auto-fit. Keyed on the
  // layout's *identity*, not the viewport size — so a new schema/layout
  // (or the first valid size) fits, but a pure resize (window resize or
  // the sidebar collapse/expand animation) preserves the user's current
  // pan/zoom. That keeps the graph — and the fixed FPS/history overlays —
  // from jumping around while the canvas width animates.
  const fittedLayoutRef = useRef<LaidNode[] | null>(null);
  const FOCUS_MIN_ZOOM = 0.9;
  useLayoutEffect(() => {
    if (laidNodes.length === 0 || size.w <= 1) return;
    if (fittedLayoutRef.current !== laidNodes) {
      fittedLayoutRef.current = laidNodes;
      const pad = 80;
      const gW = bounds.maxX - bounds.minX + pad * 2;
      const gH = bounds.maxY - bounds.minY + pad * 2;
      const k = Math.min(size.w / gW, size.h / gH, 1.4);
      const cx = (bounds.minX + bounds.maxX) / 2;
      const cy = (bounds.minY + bounds.maxY) / 2;
      viewRef.current = { x: size.w / 2 - cx * k, y: size.h / 2 - cy * k, k };
      // Seed the LOD to match the fitted zoom *now*, before the first
      // sprite build. `currentLodRef` starts at "full", so without this
      // the freshly-laid nodes get built once at full detail and only
      // get corrected a frame later when the ticker notices the fit.
      // Use clean boundaries (no hysteresis bias) — this is the initial
      // framing, not a transition between adjacent LODs.
      const fitLod: SpriteLOD =
        k >= LOD_FULL ? "full" : k >= LOD_BAR ? "bar" : "chrome";
      if (fitLod !== currentLodRef.current) {
        currentLodRef.current = fitLod;
        setLodTick((t) => t + 1);
      }
      // Arm the focus-jump bypass for the initial framing, exactly as
      // the focus-pan branch below does. The view settles to the fit on
      // frame 1 and never moves again, so without this the sprite sweep
      // runs only once: it discovers the in-view nodes, queues them for
      // *creation*, and returns. The sprites materialize on frame 2 — but
      // nothing re-enters the sweep to queue their full-LOD textures, so
      // at "full" LOD they sit on the kind placeholder forever (empty
      // card bodies). Keeping the flag set drains the create→build
      // handoff and bypasses the motion-settle gate so text appears at
      // once. (Masked at bar/chrome LOD, where the placeholder *is* the
      // intended render — which is why this only bit small schemas.)
      focusJumpPendingRef.current = true;
    }
    // Focus pan: centers + zooms to the focused type
    if (focusId && focusId !== rootId) {
      const n = nodeById.get(focusId);
      if (n) {
        const v = viewRef.current;
        const k = Math.max(v.k, FOCUS_MIN_ZOOM);
        viewRef.current = {
          k,
          x: size.w / 2 - n.cx * k,
          y: size.h / 2 - n.cy * k,
        };
        // Explicit LOD refresh — the view just jumped in one frame
        // from whatever the user was browsing to a focused-on-target
        // view at k≥FOCUS_MIN_ZOOM. Update `currentLodRef` now
        // (instead of waiting for the ticker to notice next frame)
        // and mark the build queue to bypass motion-settle so the
        // newly-in-view sprites don't sit on their low-LOD
        // placeholders for ~150 ms after the jump.
        const newLod = computeLOD(k, currentLodRef.current);
        if (newLod !== currentLodRef.current) {
          currentLodRef.current = newLod;
          setLodTick((t) => t + 1);
        }
        focusJumpPendingRef.current = true;
      }
    }
  }, [laidNodes, size, bounds, focusId, nodeById]);

  // Wheel zoom
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      wakeRef.current();
      const rect = el.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      const scale = e.deltaY < 0 ? 1.12 : 1 / 1.12;
      const v = viewRef.current;
      const k = Math.max(0.05, Math.min(4, v.k * scale));
      const ratio = k / v.k;
      viewRef.current = { k, x: mx - (mx - v.x) * ratio, y: my - (my - v.y) * ratio };
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  // Hit-testing helpers
  const screenToWorld = (clientX: number, clientY: number): Point | null => {
    const el = containerRef.current;
    if (!el) return null;
    const rect = el.getBoundingClientRect();
    const v = viewRef.current;
    return {
      x: (clientX - rect.left - v.x) / v.k,
      y: (clientY - rect.top - v.y) / v.k,
    };
  };

  // Squared distance from point P to line segment AB. Cheap version
  // used in the inner loop of edge hit-testing — compares against
  // threshold² so we never need a sqrt.
  const pointSegmentDistSq = (
    px: number, py: number,
    ax: number, ay: number,
    bx: number, by: number,
  ): number => {
    const dx = bx - ax;
    const dy = by - ay;
    const len2 = dx * dx + dy * dy;
    if (len2 < 0.0001) {
      const ex = px - ax;
      const ey = py - ay;
      return ex * ex + ey * ey;
    }
    let t = ((px - ax) * dx + (py - ay) * dy) / len2;
    t = Math.max(0, Math.min(1, t));
    const cx = ax + t * dx;
    const cy = ay + t * dy;
    const ex = px - cx;
    const ey = py - cy;
    return ex * ex + ey * ey;
  };

  /**
   * Returns the squared distance from (px, py) to the polyline
   * sampled from the edge's bezier segments. Early-exits via bbox
   * test so far-away edges cost a couple of comparisons. The sampled
   * polyline is cached per edge (WeakMap, so it dies with the edge)
   * — re-sampling the beziers on every mousemove allocated dozens of
   * Point objects per candidate edge per event.
   */
  const edgeDistSq = (px: number, py: number, edge: LaidEdge): number => {
    const pad = 16;
    if (
      px < edge.bbox.minX - pad ||
      px > edge.bbox.maxX + pad ||
      py < edge.bbox.minY - pad ||
      py > edge.bbox.maxY + pad
    ) {
      return Infinity;
    }
    const pts = edgePolyline(edge);
    let best = Infinity;
    for (let i = 2; i < pts.length; i += 2) {
      const d = pointSegmentDistSq(
        px, py,
        pts[i - 2]!, pts[i - 1]!,
        pts[i]!, pts[i + 1]!,
      );
      if (d < best) best = d;
    }
    return best;
  };

  /**
   * Find the closest edge to the cursor within EDGE_HOVER_PX screen
   * pixels. Uses the edge-tile index for early rejection — only
   * edges in the cursor's tile (and 8 neighbors) get distance-tested,
   * which keeps the cost bounded on huge schemas.
   */
  const EDGE_HOVER_PX = 6;
  // Reused between calls — hitTestEdge runs per mousemove and a fresh
  // Set allocation per event is avoidable garbage.
  const seenEdgesRef = useRef(new Set<LaidEdge>());
  const hitTestEdge = (worldX: number, worldY: number): LaidEdge | null => {
    const v = viewRef.current;
    const thresholdWorld = EDGE_HOVER_PX / v.k;
    const threshSq = thresholdWorld * thresholdWorld;
    const tcol = Math.floor(worldX / TILE_SIZE);
    const trow = Math.floor(worldY / TILE_SIZE);
    let best: LaidEdge | null = null;
    let bestD = threshSq;
    const seen = seenEdgesRef.current;
    seen.clear();
    for (let dc = -1; dc <= 1; dc++) {
      for (let dr = -1; dr <= 1; dr++) {
        const tile = edgeTilesRef.current.get(`${tcol + dc},${trow + dr}`);
        if (!tile) continue;
        for (const list of tile.groupLists) {
          for (const e of list) {
            if (seen.has(e)) continue;
            seen.add(e);
            const d = edgeDistSq(worldX, worldY, e);
            if (d < bestD) { bestD = d; best = e; }
          }
        }
      }
    }
    return best;
  };

  interface FieldHit {
    nodeId: string;
    fieldIndex: number;
    /** Name of the field if the row is a real field row. Null for
     *  interface / union member / enum-value rows where the "row" is
     *  itself the type. */
    fieldName: string | null;
    navigableTarget: string | null;
    isRelayHover: boolean;
    /** True when the pointer sits over the right-aligned return-type
     *  label (or, for union/interface rows, anywhere on the row since
     *  the row IS the type). Used to decide click action (pin field
     *  vs. navigate to return type) and to show a distinct hover. */
    isReturnTypeHover: boolean;
    /** Pixel rect (in world-space, relative to the node origin) of
     *  the return-type label — used by the hover overlay to paint a
     *  ring around just that label. Null when no label exists. */
    returnTypeRect: { x: number; y: number; w: number; h: number } | null;
  }
  const hitTestField = (worldX: number, worldY: number): FieldHit | null => {
    for (const n of nodesAt(worldX, worldY)) {
      const left = n.cx - n.w / 2;
      const right = n.cx + n.w / 2;
      const top = n.cy - n.h / 2;
      const bottom = n.cy + n.h / 2;
      if (worldX < left || worldX > right || worldY < top || worldY > bottom) continue;
      const localX = worldX - left;
      const localY = worldY - top;
      const bodyTop = n.headerH + TOP_BODY_PAD - 2;
      if (localY < bodyTop) return null;
      const rowIdx = Math.floor((localY - bodyTop) / n.rowH);
      const data = n.data;
      if (data.kind === "Object" || data.kind === "Interface" || data.kind === "Input") {
        const fields = data.fields ?? [];
        if (rowIdx < fields.length) {
          const f = fields[rowIdx]!;
          const nav =
            !BUILTIN_SCALARS.has(f.typeName) && nodeById.has(f.typeName) ? f.typeName : null;
          const isRelayHover = !!f.isRelayConnection && localX > n.w - 44;
          // Return-type label is right-aligned at `n.w - 10` in the
          // drawNodeSprite layout. Pad the hit region a few pixels so
          // the click target isn't pixel-thin.
          const typeTextW = fieldTypeTextWidth(f.type);
          const HIT_PAD_X = 4;
          const relayW = f.isRelayConnection ? 12 : 0;
          const rtLeft = n.w - 10 - typeTextW - relayW - HIT_PAD_X;
          const rtRight = n.w - 10 + HIT_PAD_X;
          const isReturnTypeHover = localX >= rtLeft && localX <= rtRight;
          const rowY = bodyTop + rowIdx * n.rowH;
          return {
            nodeId: n.id,
            fieldIndex: rowIdx,
            fieldName: f.name,
            navigableTarget: nav,
            isRelayHover,
            isReturnTypeHover,
            returnTypeRect: {
              x: rtLeft,
              y: rowY,
              w: rtRight - rtLeft,
              h: n.rowH,
            },
          };
        }
        // Trailing sections (implements / member-of-union) use the
        // tight ROW_H pitch and carry gap/centering offsets — derive
        // the row index from the painter's exact geometry instead of
        // the field grid.
        const interfaces = data.interfaces ?? [];
        const geom = trailingSectionGeom(n);
        const ifaceIdx =
          localY >= geom.ifaceRowsTop &&
          localY < geom.ifaceRowsTop + geom.ifaceBlockH
            ? Math.floor((localY - geom.ifaceRowsTop) / ROW_H)
            : -1;
        if (ifaceIdx >= 0 && ifaceIdx < interfaces.length) {
          const ifaceName = interfaces[ifaceIdx]!;
          const nav = nodeById.has(ifaceName) ? ifaceName : null;
          return {
            nodeId: n.id,
            fieldIndex: fields.length + ifaceIdx,
            fieldName: null,
            navigableTarget: nav,
            isRelayHover: false,
            // Implements rows are the type itself — the whole row
            // navigates and shows the return-type hover treatment.
            isReturnTypeHover: !!nav,
            returnTypeRect: null,
          };
        }
        // "Member of union" rows follow the implements section and
        // behave the same way: the row IS the union type.
        const memberOfUnions =
          data.kind === "Object" ? (data.memberOfUnions ?? []) : [];
        const unionIdx =
          localY >= geom.unionRowsTop &&
          localY < geom.unionRowsTop + geom.unionBlockH
            ? Math.floor((localY - geom.unionRowsTop) / ROW_H)
            : -1;
        if (unionIdx >= 0 && unionIdx < memberOfUnions.length) {
          const unionName = memberOfUnions[unionIdx]!;
          const nav = nodeById.has(unionName) ? unionName : null;
          return {
            nodeId: n.id,
            fieldIndex: fields.length + interfaces.length + unionIdx,
            fieldName: null,
            navigableTarget: nav,
            isRelayHover: false,
            isReturnTypeHover: !!nav,
            returnTypeRect: null,
          };
        }
        return null;
      }
      if (data.kind === "Union") {
        // Member rows use the tight ROW_H pitch (no descriptions).
        const memberIdx = Math.floor((localY - bodyTop) / ROW_H);
        const m = data.members?.[memberIdx];
        if (!m) return null;
        const nav = !BUILTIN_SCALARS.has(m) && nodeById.has(m) ? m : null;
        return {
          nodeId: n.id,
          fieldIndex: memberIdx,
          fieldName: null,
          navigableTarget: nav,
          isRelayHover: false,
          isReturnTypeHover: !!nav,
          returnTypeRect: null,
        };
      }
      if (data.kind === "Enum") {
        const v = data.values?.[rowIdx];
        if (!v) return null;
        return {
          nodeId: n.id,
          fieldIndex: rowIdx,
          fieldName: null,
          navigableTarget: null,
          isRelayHover: false,
          isReturnTypeHover: false,
          returnTypeRect: null,
        };
      }
      return null;
    }
    return null;
  };

  // Latest hit-test closure for the once-registered debug hook (its
  // effect would otherwise capture the pre-layout closure).
  const hitTestFieldRef = useRef(hitTestField);
  hitTestFieldRef.current = hitTestField;

  const hitTestNodeHeader = (worldX: number, worldY: number): string | null => {
    for (const n of nodesAt(worldX, worldY)) {
      const left = n.cx - n.w / 2;
      const right = n.cx + n.w / 2;
      const top = n.cy - n.h / 2;
      if (worldX >= left && worldX <= right && worldY >= top && worldY <= top + n.headerH) {
        return n.id;
      }
    }
    return null;
  };

  const hitTestNode = (worldX: number, worldY: number): string | null => {
    for (const n of nodesAt(worldX, worldY)) {
      if (
        worldX >= n.cx - n.w / 2 &&
        worldX <= n.cx + n.w / 2 &&
        worldY >= n.cy - n.h / 2 &&
        worldY <= n.cy + n.h / 2
      ) return n.id;
    }
    return null;
  };

  const onMouseDown = (e: React.MouseEvent) => {
    wakeRef.current();
    dragRef.current = {
      active: true,
      lastX: e.clientX,
      lastY: e.clientY,
      startX: e.clientX,
      startY: e.clientY,
      moved: false,
    };
  };

  const onMouseMove = (e: React.MouseEvent) => {
    wakeRef.current();
    const drag = dragRef.current;
    if (drag.active) {
      const dx = e.clientX - drag.lastX;
      const dy = e.clientY - drag.lastY;
      drag.lastX = e.clientX;
      drag.lastY = e.clientY;
      if (
        Math.abs(e.clientX - drag.startX) > CLICK_DRAG_THRESHOLD ||
        Math.abs(e.clientY - drag.startY) > CLICK_DRAG_THRESHOLD
      ) {
        drag.moved = true;
      }
      const v = viewRef.current;
      viewRef.current = { ...v, x: v.x + dx, y: v.y + dy };
      return;
    }
    const world = screenToWorld(e.clientX, e.clientY);
    if (!world) {
      hoveredFieldRef.current = null;
      return;
    }
    const hit = hitTestField(world.x, world.y);
    const hoveredNode = hitTestNode(world.x, world.y);
    if (onNavigate) {
      const lowLod =
      currentLodRef.current !== "full" ||
      viewRef.current.k < FIELD_CLICK_MIN_ZOOM;
      // At low LOD the whole node card is the click target. At full
      // LOD the pointer appears whenever the cursor is on a field row
      // (which is always clickable — pins the field) or on a node
      // header (navigates to that node).
      const fullLodPointer = !!hit || !!hitTestNodeHeader(world.x, world.y);
      setCursor((lowLod ? !!hoveredNode : fullLodPointer) ? "pointer" : "grab");
    }
    const prev = hoveredFieldRef.current;
    const same =
      prev !== null &&
      hit !== null &&
      prev.nodeId === hit.nodeId &&
      prev.fieldIndex === hit.fieldIndex &&
      prev.isRelayHover === hit.isRelayHover &&
      prev.isReturnTypeHover === hit.isReturnTypeHover;
    if (!same) {
      hoveredFieldRef.current = hit
        ? {
            nodeId: hit.nodeId,
            fieldIndex: hit.fieldIndex,
            isRelayHover: hit.isRelayHover,
            isReturnTypeHover: hit.isReturnTypeHover,
            returnTypeRect: hit.returnTypeRect,
          }
        : null;
    }
    hoveredNodeRef.current = hoveredNode;

    // Node-name tooltip data — always tracked here; the JSX shows it
    // only at low LODs (bar / chrome) where the sprite doesn't paint
    // the name. State update is gated on id changes so we don't
    // re-render on every mouse move while parked over one node.
    if (hoveredNode !== hoveredNodeForTipRef.current) {
      hoveredNodeForTipRef.current = hoveredNode;
      if (hoveredNode) {
        const n = nodeById.get(hoveredNode);
        if (n) setHoveredNodeTip({ name: n.data.name, kind: n.data.kind });
        else setHoveredNodeTip(null);
      } else {
        setHoveredNodeTip(null);
      }
    }
    if (hoveredNode) {
      hoveredNodeScreenRef.current = { x: e.clientX, y: e.clientY };
      if (nodeTipElRef.current) {
        applyTooltipStyle(nodeTipElRef.current, e.clientX, e.clientY);
      }
    }

    // Edge hover — only check when the cursor isn't already over a
    // node card (the node would otherwise occlude the edge endpoint
    // and edge hover would feel sticky on the node).
    const edge = hoveredNode ? null : hitTestEdge(world.x, world.y);
    const prevEdge = hoveredEdgeRef.current;
    if (edge !== prevEdge) {
      hoveredEdgeRef.current = edge;
      if (edge && edge.label) {
        setHoveredEdgeInfo({
          label: edge.label,
          sourceId: edge.sourceId,
          targetId: edge.targetId,
          kind: edge.kind,
        });
      } else {
        setHoveredEdgeInfo(null);
      }
    }
    if (edge && edge.label) {
      hoveredEdgeScreenRef.current = { x: e.clientX, y: e.clientY };
      if (edgeTipElRef.current) {
        applyTooltipStyle(edgeTipElRef.current, e.clientX, e.clientY);
      }
    }
  };

  const endDrag = () => {
    wakeRef.current();
    dragRef.current.active = false;
    hoveredFieldRef.current = null;
    hoveredNodeRef.current = null;
    if (hoveredEdgeRef.current !== null) {
      hoveredEdgeRef.current = null;
      setHoveredEdgeInfo(null);
    }
    if (hoveredNodeForTipRef.current !== null) {
      hoveredNodeForTipRef.current = null;
      setHoveredNodeTip(null);
    }
  };

  const onClick = (e: React.MouseEvent) => {
    if (dragRef.current.moved) return;
    const world = screenToWorld(e.clientX, e.clientY);
    if (!world) return;
    const recordNodeClick = (id: string) => {
      const n = nodeById.get(id);
      if (n) {
        pushHistory({
          kind: "node",
          id: `node:${id}`,
          nodeId: id,
          name: n.data.name,
          nodeKind: n.data.kind,
          ts: Date.now(),
        });
      }
    };
    const lowLod =
      currentLodRef.current !== "full" ||
      viewRef.current.k < FIELD_CLICK_MIN_ZOOM;
    if (lowLod) {
      // Field text is unreadable here, so the whole node card is the
      // click target. Click frames the node — zooms in and centers
      // so the user can immediately read its fields.
      const nodeId = hitTestNode(world.x, world.y);
      if (nodeId) {
        setFocusedEdge(null);
        recordNodeClick(nodeId);
        const n = nodeById.get(nodeId);
        if (n) {
          const pad = 120;
          const fitK = Math.min(
            size.w / (n.w + pad * 2),
            size.h / (n.h + pad * 2),
            1.4,
          );
          const targetK = Math.max(FIELD_CLICK_MIN_ZOOM * 1.6, fitK);
          viewRef.current = {
            k: targetK,
            x: size.w / 2 - n.cx * targetK,
            y: size.h / 2 - n.cy * targetK,
          };
        }
        return;
      }
    } else {
      const hit = hitTestField(world.x, world.y);
      if (hit) {
        // Click on the right-aligned return-type label → navigate to
        // the field's target type. Anywhere else on the row →
        // pin the field and frame the canvas onto its owner type so
        // the user can keep inspecting the source while seeing the
        // highlight stay on the row.
        if (hit.isReturnTypeHover && hit.navigableTarget) {
          setFocusedEdge(null);
          recordNodeClick(hit.navigableTarget);
          onNavigate?.(hit.navigableTarget);
          return;
        }
        if (hit.fieldName) {
          setFocusedEdge(null);
          setPinnedField({
            typeId: hit.nodeId,
            fieldName: hit.fieldName,
            fieldIndex: hit.fieldIndex,
          });
          return;
        }
        // Union/interface row without a separate "name" — click acts
        // as navigate when target is available.
        if (hit.navigableTarget) {
          setFocusedEdge(null);
          recordNodeClick(hit.navigableTarget);
          onNavigate?.(hit.navigableTarget);
          return;
        }
      }
      const nodeId = hitTestNodeHeader(world.x, world.y);
      if (nodeId) { setFocusedEdge(null); recordNodeClick(nodeId); onNavigate?.(nodeId); return; }
    }
    // Edge click — frame the view so both endpoint nodes are visible
    // and the edge's midpoint is at screen center. Doesn't navigate,
    // so the user can keep their focus context intact.
    const edge = hitTestEdge(world.x, world.y);
    if (edge) {
      focusOnEdge(edge);
      return;
    }
    setFocusedEdge(null);
    setPinnedField(null);
    onClearFocus?.();
  };

  // Touch gestures — wrap the click-target hit test so a tap at low
  // LOD selects the whole node card (where field text isn't drawn).
  // Returns either a navigation target id, a pin instruction, or null.
  type TapAction =
    | { kind: "navigate"; id: string }
    | { kind: "pin"; typeId: string; fieldName: string; fieldIndex: number }
    | null;
  const tapHitTest = (wx: number, wy: number): TapAction => {
    if (
      currentLodRef.current !== "full" ||
      viewRef.current.k < FIELD_CLICK_MIN_ZOOM
    ) {
      const id = hitTestNode(wx, wy);
      return id ? { kind: "navigate", id } : null;
    }
    const hit = hitTestField(wx, wy);
    if (hit) {
      if (hit.isReturnTypeHover && hit.navigableTarget) {
        return { kind: "navigate", id: hit.navigableTarget };
      }
      if (hit.fieldName) {
        return {
          kind: "pin",
          typeId: hit.nodeId,
          fieldName: hit.fieldName,
          fieldIndex: hit.fieldIndex,
        };
      }
      if (hit.navigableTarget) {
        return { kind: "navigate", id: hit.navigableTarget };
      }
    }
    const header = hitTestNodeHeader(wx, wy);
    return header ? { kind: "navigate", id: header } : null;
  };
  const hitTestRef = useRef(tapHitTest);
  hitTestRef.current = tapHitTest;
  const onNavigateRef = useRef(onNavigate);
  onNavigateRef.current = onNavigate;
  const setPinnedFieldRef = useRef(setPinnedField);
  setPinnedFieldRef.current = setPinnedField;

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    type Pt = { x: number; y: number; startX: number; startY: number };
    const points = new Map<number, Pt>();
    let mode: "none" | "pan" | "pinch" = "none";
    let panMoved = false;
    let pinchStartDist = 0;
    let pinchStartK = 1;

    const enterPan = () => { mode = "pan"; panMoved = false; };
    const enterPinch = () => {
      const arr = [...points.values()];
      if (arr.length < 2) return;
      pinchStartDist = Math.hypot(arr[0]!.x - arr[1]!.x, arr[0]!.y - arr[1]!.y);
      pinchStartK = viewRef.current.k;
      mode = "pinch";
    };

    const onTouchStart = (e: TouchEvent) => {
      if (e.cancelable) e.preventDefault();
      wakeRef.current();
      for (let i = 0; i < e.changedTouches.length; i++) {
        const t = e.changedTouches[i]!;
        points.set(t.identifier, { x: t.clientX, y: t.clientY, startX: t.clientX, startY: t.clientY });
      }
      if (points.size === 1) enterPan();
      else if (points.size >= 2) enterPinch();
    };

    const onTouchMove = (e: TouchEvent) => {
      if (e.cancelable) e.preventDefault();
      wakeRef.current();
      const before = new Map<number, { x: number; y: number }>();
      for (const [id, p] of points) before.set(id, { x: p.x, y: p.y });
      for (let i = 0; i < e.changedTouches.length; i++) {
        const t = e.changedTouches[i]!;
        const existing = points.get(t.identifier);
        if (!existing) continue;
        existing.x = t.clientX;
        existing.y = t.clientY;
      }
      if (mode === "pan" && points.size === 1) {
        const arr = [...points.entries()];
        const [id, pt] = arr[0]!;
        const prev = before.get(id);
        if (!prev) return;
        const dx = pt.x - prev.x;
        const dy = pt.y - prev.y;
        if (
          Math.abs(pt.x - pt.startX) > CLICK_DRAG_THRESHOLD ||
          Math.abs(pt.y - pt.startY) > CLICK_DRAG_THRESHOLD
        ) panMoved = true;
        const v = viewRef.current;
        viewRef.current = { ...v, x: v.x + dx, y: v.y + dy };
        return;
      }
      if (mode === "pinch" && points.size >= 2) {
        const arr = [...points.values()];
        const a = arr[0]!, b = arr[1]!;
        const dist = Math.hypot(a.x - b.x, a.y - b.y);
        if (pinchStartDist <= 0) return;
        const newK = Math.max(0.05, Math.min(4, pinchStartK * (dist / pinchStartDist)));
        const rect = el.getBoundingClientRect();
        const cx = (a.x + b.x) / 2 - rect.left;
        const cy = (a.y + b.y) / 2 - rect.top;
        const v = viewRef.current;
        const ratio = newK / v.k;
        viewRef.current = { k: newK, x: cx - (cx - v.x) * ratio, y: cy - (cy - v.y) * ratio };
      }
    };

    const onTouchEnd = (e: TouchEvent) => {
      wakeRef.current();
      const ended: Touch[] = [];
      for (let i = 0; i < e.changedTouches.length; i++) {
        const t = e.changedTouches[i]!;
        if (points.has(t.identifier)) { ended.push(t); points.delete(t.identifier); }
      }
      if (mode === "pinch" && points.size === 1) {
        const remaining = [...points.values()][0]!;
        remaining.startX = remaining.x;
        remaining.startY = remaining.y;
        enterPan();
        return;
      }
      if (points.size === 0) {
        const wasTap = mode === "pan" && !panMoved && ended.length > 0;
        mode = "none";
        if (!wasTap) return;
        const t = ended[ended.length - 1]!;
        const rect = el.getBoundingClientRect();
        const v = viewRef.current;
        const wx = (t.clientX - rect.left - v.x) / v.k;
        const wy = (t.clientY - rect.top - v.y) / v.k;
        const action = hitTestRef.current(wx, wy);
        if (!action) return;
        if (action.kind === "navigate") {
          onNavigateRef.current?.(action.id);
        } else {
          setPinnedFieldRef.current({
            typeId: action.typeId,
            fieldName: action.fieldName,
            fieldIndex: action.fieldIndex,
          });
        }
      }
    };

    el.addEventListener("touchstart", onTouchStart, { passive: false });
    el.addEventListener("touchmove", onTouchMove, { passive: false });
    el.addEventListener("touchend", onTouchEnd, { passive: false });
    el.addEventListener("touchcancel", onTouchEnd, { passive: false });
    return () => {
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      el.removeEventListener("touchend", onTouchEnd);
      el.removeEventListener("touchcancel", onTouchEnd);
    };
  }, []);

  // ─── Pixi Application init ────────────────────────────────────────

  useEffect(() => {
    const mountEl = pixiContainerRef.current;
    if (!mountEl) return;

    // Cap framebuffer resolution at 2× regardless of monitor DPR. A
    // retina-3x 4K display at native resolution is a ~115 MB backbuffer
    // before any content, which alone can push the GPU process over its
    // per-tab budget and fire "Aw, Snap!" on first render.
    const dpr = Math.min(2, window.devicePixelRatio || 1);
    // Use themeResolved (from React context) for the first-frame
    // background. We can't read CSS vars here because React fires
    // child effects before parent — ThemeProvider hasn't toggled the
    // .dark class on <html> yet, so getComputedStyle still returns
    // the light-mode value. The background sync effect (below) picks
    // up the real CSS var once both the app and the theme are ready.
    const initBgHex = themeResolved === "dark" ? 0x0a0a0a : 0xffffff;
    const app = new Application();

    let destroyed = false;
    app.init({
      width: size.w,
      height: size.h,
      resolution: dpr,
      autoDensity: true,
      // Framebuffer MSAA is off: edges are analytically feathered
      // meshes, arrowheads are feathered-texture sprites, and node
      // cards / grid are textures — none need multisampling. The only
      // remaining raw vector geometry is the axis-aligned rounded-rect
      // overlays (focus ring, hover highlight, investigate outline),
      // which alias mildly at the corners only. Skipping MSAA frees
      // multisample fill-rate across the whole canvas (the 120fps
      // budget's biggest fixed GPU cost).
      antialias: false,
      backgroundAlpha: 1,
      backgroundColor: initBgHex,
      preference: "webgl",
    }).then(() => {
      if (destroyed) {
        app.destroy(true);
        return;
      }

      mountEl.appendChild(app.canvas as HTMLCanvasElement);

      // Scene graph
      // Build the dot grid texture immediately so the grid doesn't
      // start as a solid white Texture.WHITE covering the dark bg.
      const initMutedFg = getComputedCssVar("--muted-foreground", "#64748b");
      const initGridTex = buildDotGridTexture(cssColorToHex(initMutedFg), 0.18);
      const gridTiling = new TilingSprite({
        texture: initGridTex,
        width: size.w,
        height: size.h,
      });
      gridTiling.tileScale.set(1);

      const world = new Container();
      world.cullable = true;

      const edgeTileContainer = new Container();
      // Full-alpha copies of the currently-"active" (focused) edges.
      // The tile containers get dimmed wholesale via alpha when a
      // focus is set, and the few active edges are redrawn here on
      // top — so focus changes never rebuild tile geometry.
      const activeEdgeContainer = new Container();
      const focusEdgeGraphics = new Container();
      const hoverEdgeGraphics = new Container();
      const nodeContainer = new Container();
      nodeContainer.cullable = true;
      // SDF text layer — glyph quad meshes above the card chrome.
      const textContainer = new Container();
      textContainer.cullable = true;
      const investigateOverlay = new Graphics();
      const pinFieldGraphics = new Graphics();
      const hoverGraphics = new Graphics();
      const focusGraphics = new Graphics();

      world.addChild(edgeTileContainer);
      world.addChild(activeEdgeContainer);
      // Focused-edge bold stroke sits above the edge tiles so the
      // selected line reads thicker than its neighbors. Hover overlay
      // goes on top of focus so hovering still paints the brighter
      // emphasis even when an edge is already focused.
      world.addChild(focusEdgeGraphics);
      // Highlight is between edges and nodes — drawn on top of every
      // other edge in the tiles but covered by node cards, so the
      // emphasized line reads cleanly without spilling onto nodes.
      world.addChild(hoverEdgeGraphics);
      world.addChild(nodeContainer);
      // Text sits directly above the card sprites so field highlights
      // (pin/hover, added later) keep their existing z-order relative
      // to the chrome.
      world.addChild(textContainer);
      // Investigate overlay sits above nodes so its orange outlines
      // pop over the (possibly dimmed) cards.
      world.addChild(investigateOverlay);
      // Pinned-field highlight sits above the node sprite so it's
      // visible on top of the field text, but below the focus ring
      // so focus state stays the dominant visual.
      world.addChild(pinFieldGraphics);
      world.addChild(hoverGraphics);
      world.addChild(focusGraphics);

      app.stage.addChild(gridTiling);
      app.stage.addChild(world);

      appRef.current = app;
      setAppReady(true);
      sceneRef.current = {
        gridTiling,
        world,
        edgeTileContainer,
        activeEdgeContainer,
        focusEdgeGraphics,
        hoverEdgeGraphics,
        nodeContainer,
        textContainer,
        investigateOverlay,
        pinFieldGraphics,
        hoverGraphics,
        focusGraphics,
      };

      // Build one shared placeholder texture per NodeKind. Sprites at
      // chrome/bar LOD point at the matching entry so every colored
      // card shares the same upload instead of us uploading 1,400
      // of them.
      for (const kind of Object.keys(KIND_COLORS) as NodeKind[]) {
        kindTextureCacheRef.current.set(
          kind,
          buildKindPlaceholderTexture(kind),
        );
      }

      // FPS tick counter
      let fpsTimes: number[] = [];
      let lastFpsSampleAt = 0;

      // Render-on-demand: after IDLE_STOP_FRAMES consecutive frames
      // with no pending work (no view change, no queued sprite/tile
      // builds, no running animation) the ticker stops entirely — a
      // fully static canvas costs zero GPU/CPU per frame. Anything
      // that can change the scene calls `wake()`: every React render
      // (see the deps-less effect below) plus the raw pointer/wheel/
      // touch handlers that mutate viewRef without rendering.
      const IDLE_STOP_FRAMES = 60;
      let idleFrames = 0;
      let lastView = { x: NaN, y: NaN, k: NaN };
      // Frame pacing state — EMA of the real display interval so the
      // per-frame work budgets below scale to the refresh rate (a
      // 120 Hz frame is ~8.3 ms; budgets tuned for 60 Hz would
      // guarantee dropped frames whenever queues are draining).
      let lastFrameAt = 0;
      let frameIntervalEma = 16.7;
      wakeRef.current = () => {
        idleFrames = 0;
        if (!app.ticker.started) app.ticker.start();
      };

      app.ticker.add(() => {
        const scene = sceneRef.current;
        if (!scene.world) return;

        const v = viewRef.current;
        const sw = app.screen.width;
        const sh = app.screen.height;

        const viewChanged =
          v.x !== lastView.x || v.y !== lastView.y || v.k !== lastView.k;
        lastView = { x: v.x, y: v.y, k: v.k };
        let tileWork = false;
        let animActive = false;

        // Shared work deadline for all progressive builders this
        // frame (tile tessellation, sprite creation, texture draws):
        // ~35% of the measured frame interval, leaving the rest for
        // Pixi's render pass. At 60 Hz this is ~5 ms (close to the
        // old fixed budgets); at 120 Hz it tightens to ~3 ms so the
        // same work spreads over more frames instead of blowing the
        // 8.3 ms budget. The texel budget separately caps GPU upload
        // volume — canvas draw time is bounded by the deadline, but
        // upload cost at render time scales with texture area.
        const frameStart = performance.now();
        if (lastFrameAt > 0) {
          const dt = Math.min(50, frameStart - lastFrameAt);
          frameIntervalEma = frameIntervalEma * 0.9 + dt * 0.1;
        }
        lastFrameAt = frameStart;
        const workDeadline =
          frameStart + Math.min(5, Math.max(1, frameIntervalEma * 0.35));
        const texelBudget = Math.min(
          4_000_000,
          Math.max(1_000_000, frameIntervalEma * 150_000),
        );

        // Sync world transform
        scene.world.position.set(v.x, v.y);
        scene.world.scale.set(v.k, v.k);
        updateEdgeZoom(v.k);
        if (SDF_TEXT_ENABLED) updateTextZoom(v.k);

        // Detect LOD change → trigger node/edge rebuild. Whenever
        // the LOD steps up into "full" (most often: right after the
        // initial auto-fit resolves the user's first zoom-in, or
        // when zooming back in from chrome / bar), arm the focus-
        // jump bypass so the texture build queue drains on the
        // next frame instead of waiting out the 150 ms motion-
        // settle gate. Without this the freshly-mounted canvas
        // sits on kind-placeholder textures for an extra beat
        // after the user starts inspecting it.
        const newLod = computeLOD(v.k, currentLodRef.current);
        if (newLod !== currentLodRef.current) {
          const prevLod = currentLodRef.current;
          currentLodRef.current = newLod;
          setLodTick((t) => t + 1);
          if (newLod === "full" && prevLod !== "full") {
            focusJumpPendingRef.current = true;
          }
        }

        // Sync grid tiling
        if (scene.gridTiling) {
          scene.gridTiling.width = sw;
          scene.gridTiling.height = sh;
          scene.gridTiling.tilePosition.set(v.x % 24, v.y % 24);
          scene.gridTiling.tileScale.set(v.k, v.k);
        }

        // Hover highlight
        if (scene.hoverGraphics) {
          scene.hoverGraphics.clear();
          const hoveredField = hoveredFieldRef.current;
          if (hoveredField) {
            const n = nodeByIdRef.current.get(hoveredField.nodeId);
            if (n) {
              // Theme-resolved foreground color, cached per theme change
              // — calling getComputedStyle here forced a style recalc
              // every frame while a field was hovered.
              const fgHex = fgHexRef.current;
              const nodeLeft = n.cx - n.w / 2;
              const nodeTop = n.cy - n.h / 2;
              const bodyTop = n.headerH + TOP_BODY_PAD - 2;
              const fields = n.data.fields ?? [];
              const interfaces = n.data.interfaces ?? [];
              const memberOfUnions =
                n.data.kind === "Object" ? (n.data.memberOfUnions ?? []) : [];
              let hy: number;
              // Rows without description lines (implements /
              // member-of-union sections, Union member rows) use the
              // tight ROW_H pitch. Their Y comes from the shared
              // painter geometry so the highlight tracks the rendered
              // text exactly.
              let hoverRowH = n.rowH;
              if (n.data.kind === "Union") {
                hy = nodeTop + bodyTop + hoveredField.fieldIndex * ROW_H;
                hoverRowH = ROW_H;
              } else if (
                hoveredField.fieldIndex >= fields.length + interfaces.length &&
                memberOfUnions.length > 0
              ) {
                const unionIdx =
                  hoveredField.fieldIndex - fields.length - interfaces.length;
                hy =
                  nodeTop + trailingSectionGeom(n).unionRowsTop + unionIdx * ROW_H;
                hoverRowH = ROW_H;
              } else if (
                hoveredField.fieldIndex >= fields.length &&
                interfaces.length > 0
              ) {
                const ifaceIdx = hoveredField.fieldIndex - fields.length;
                hy =
                  nodeTop + trailingSectionGeom(n).ifaceRowsTop + ifaceIdx * ROW_H;
                hoverRowH = ROW_H;
              } else {
                hy = nodeTop + bodyTop + hoveredField.fieldIndex * n.rowH;
              }
              const hpad = 4;
              scene.hoverGraphics.roundRect(nodeLeft + hpad, hy, n.w - hpad * 2, hoverRowH, 3);
              scene.hoverGraphics.fill({ color: fgHex, alpha: 0.07 });

              // Distinct return-type hover effect — only fires when
              // the pointer is on the right-aligned type label of a
              // field row. Drawn as a colored fill + soft outline
              // around just the type chip so users see the type as a
              // separate click target (click → navigate) vs. the rest
              // of the row (click → pin field on owner type).
              if (hoveredField.isReturnTypeHover && hoveredField.returnTypeRect) {
                const r = hoveredField.returnTypeRect;
                scene.hoverGraphics.roundRect(
                  nodeLeft + r.x,
                  nodeTop + r.y,
                  r.w,
                  r.h,
                  3,
                );
                scene.hoverGraphics.fill({ color: 0xf59e0b, alpha: 0.22 });
                scene.hoverGraphics.roundRect(
                  nodeLeft + r.x,
                  nodeTop + r.y,
                  r.w,
                  r.h,
                  3,
                );
                scene.hoverGraphics.stroke({ width: 1, color: 0xf59e0b, alpha: 0.9 });
              }
            }
          }
        }

        // Focus + hover rings
        if (scene.focusGraphics) {
          scene.focusGraphics.clear();

          const hoveredNodeId = hoveredNodeRef.current;
          const focusIdVal = focusIdRef.current;

          if (hoveredNodeId && hoveredNodeId !== focusIdVal) {
            const n = nodeByIdRef.current.get(hoveredNodeId);
            if (n) {
              const colorStr = KIND_COLORS[n.data.kind];
              const colorHex = cssColorToHex(colorStr);
              const pad = 3;
              scene.focusGraphics.roundRect(
                n.cx - n.w / 2 - pad, n.cy - n.h / 2 - pad,
                n.w + pad * 2, n.h + pad * 2, 9,
              );
              scene.focusGraphics.stroke({ width: 1.5, color: colorHex, alpha: 0.4 });
            }
          }

          if (focusIdVal) {
            const n = nodeByIdRef.current.get(focusIdVal);
            if (n) {
              const colorStr = KIND_COLORS[n.data.kind];
              const colorHex = cssColorToHex(colorStr);

              // Ripple animation is time-limited (a few cycles after the
              // focus change) so a parked focus doesn't keep the ticker
              // alive forever — after it finishes only the static ring
              // remains and the canvas can go idle.
              const RIPPLE_CYCLE_MS = 1600;
              const RIPPLE_TOTAL_MS = 4800;
              const elapsed = performance.now() - focusChangedAtRef.current;
              if (elapsed < RIPPLE_TOTAL_MS) {
                animActive = true;
                const t = (elapsed % RIPPLE_CYCLE_MS) / RIPPLE_CYCLE_MS;
                const ripplePad = t * 18;
                const rippleAlpha = (1 - t) * 0.6;

                scene.focusGraphics.roundRect(
                  n.cx - n.w / 2 - ripplePad, n.cy - n.h / 2 - ripplePad,
                  n.w + ripplePad * 2, n.h + ripplePad * 2, 6 + ripplePad,
                );
                scene.focusGraphics.stroke({ width: 2, color: colorHex, alpha: rippleAlpha });
              }

              const pad = 3;
              scene.focusGraphics.roundRect(
                n.cx - n.w / 2 - pad, n.cy - n.h / 2 - pad,
                n.w + pad * 2, n.h + pad * 2, 9,
              );
              scene.focusGraphics.stroke({ width: 2.5, color: colorHex, alpha: 0.75 });
            }
          }
        }

        // Edge tile visibility + lazy build. Per frame: step the
        // counter, intersect each tile with the padded viewport,
        // lazy-build the tile's Graphics on first visit, and evict
        // tiles that have been off-screen for long enough to free GPU
        // memory. This is the core mobile-memory fix — a monolithic
        // Graphics holding all 10k+ edges at once overflows WebGL
        // vertex budgets on low-end devices.
        frameCounterRef.current++;
        const tiles = edgeTilesRef.current;
        if (tiles.size > 0 && scene.edgeTileContainer) {
          const viewMinX = -v.x / v.k - TILE_VIEW_PADDING;
          const viewMinY = -v.y / v.k - TILE_VIEW_PADDING;
          const viewMaxX = (sw - v.x) / v.k + TILE_VIEW_PADDING;
          const viewMaxY = (sh - v.y) / v.k + TILE_VIEW_PADDING;

          const frame = frameCounterRef.current;
          // Tile lazy-builds share the frame's work deadline. A
          // zoom-out or sudden pan can pull many tiles into view at
          // once; without the cap they'd all tessellate in the same
          // frame and drop 30–60 ms. Also gated on motion settle —
          // the same mobile GPU driver that dies from rapid texture
          // uploads also dies from rapid vertex-buffer uploads during
          // a fast pan.
          const tileStable =
            focusJumpPendingRef.current ||
            performance.now() - lastViewChangeAtRef.current >= MOTION_SETTLE_MS;
          const buildDeadline = workDeadline;
          let batchesBuiltThisFrame = 0;

          for (const tile of tiles.values()) {
            const tileMinX = tile.col * TILE_SIZE;
            const tileMinY = tile.row * TILE_SIZE;
            const tileMaxX = tileMinX + TILE_SIZE;
            const tileMaxY = tileMinY + TILE_SIZE;
            const intersects = !(
              tileMaxX < viewMinX ||
              tileMinX > viewMaxX ||
              tileMaxY < viewMinY ||
              tileMinY > viewMaxY
            );

            if (intersects) {
              tile.lastSeenFrame = frame;
              // Lazily compute the batch plan the first time this tile
              // shows up — saves the count work for tiles that never
              // become visible.
              if (tile.totalBatches < 0) {
                tile.totalBatches = plannedBatchCount(tile);
              }
              // Build outstanding batches under the shared per-frame
              // budget. Each iteration appends one Graphics; partial
              // builds render as "edges fading in" — usually completes
              // in 1-3 frames for typical tile density.
              while (
                tile.builtBatches < tile.totalBatches &&
                tileStable &&
                performance.now() < buildDeadline &&
                batchesBuiltThisFrame < TILE_BATCH_BUDGET_PER_FRAME
              ) {
                const built = buildEdgeTileBatch(tile, tile.builtBatches);
                tile.builtBatches += 1;
                batchesBuiltThisFrame += 1;
                if (!built) continue;
                tile.edgeBatches.push(built);
                scene.edgeTileContainer.addChild(built);
              }
              for (const g of tile.edgeBatches) g.visible = true;
            } else {
              for (const g of tile.edgeBatches) g.visible = false;
              if (
                tile.edgeBatches.length > 0 &&
                frame - tile.lastSeenFrame > TILE_EVICT_FRAMES
              ) {
                for (const g of tile.edgeBatches) g.destroy({ children: true });
                tile.edgeBatches = [];
                tile.builtBatches = 0;
                // Mark for re-planning on next visibility — the group
                // lists could change before the tile comes back into
                // view (e.g. focus moved).
                tile.totalBatches = -1;
              }
            }
          }
          if (batchesBuiltThisFrame > 0) tileWork = true;
        }

        // Progressive sprite creation drain. The sweep below can
        // enqueue thousands of nodes needing a Sprite in one pass;
        // draining them here with a small per-frame budget keeps
        // `new Sprite` + `addChild` load bounded per frame and
        // prevents the Pixi renderer from stalling out.
        const spriteCreateQueue = spriteCreateQueueRef.current;
        if (
          spriteCreateQueue &&
          spriteCreateQueue.length > 0 &&
          scene.nodeContainer
        ) {
          const nodeContainer = scene.nodeContainer;
          const createDeadline = workDeadline;
          while (
            spriteCreateQueue.length > 0 &&
            performance.now() < createDeadline
          ) {
            const node = spriteCreateQueue.pop()!;
            if (nodeSpritesRef.current.has(node.id)) continue;
            const kindTex = kindTextureCacheRef.current.get(node.data.kind);
            const usingPlaceholder = !!kindTex;
            const sprite = new NineSliceSprite({
              texture: kindTex ?? Texture.WHITE,
              leftWidth: usingPlaceholder ? PLACEHOLDER_CORNER : 0,
              topHeight: usingPlaceholder ? PLACEHOLDER_HEADER_H : 0,
              rightWidth: usingPlaceholder ? PLACEHOLDER_CORNER : 0,
              bottomHeight: usingPlaceholder ? PLACEHOLDER_CORNER : 0,
              width: node.w,
              height: node.h,
            });
            sprite.position.set(node.cx - node.w / 2, node.cy - node.h / 2);
            sprite.cullable = true;
            if (!kindTex) {
              sprite.tint = cssColorToHex(KIND_COLORS[node.data.kind]);
            }
            // Apply current focus-dim state so sprites created mid-
            // focus (e.g. when the post-click sweep finally brings an
            // endpoint into view) don't briefly flash at full alpha.
            if (edgeGroupsRef.current.dimNodeIds.has(node.id)) {
              sprite.alpha = 0.1;
            }
            nodeContainer.addChild(sprite);
            nodeSpritesRef.current.set(node.id, sprite);
          }
          if (spriteCreateQueue.length === 0) {
            spriteCreateQueueRef.current = null;
          }
        }

        // Sprite viewport sweep. Runs on any significant view change
        // (pan/zoom/LOD). Sprites are queued for progressive creation
        // (only for nodes currently in the padded viewport) and
        // destroyed once off-screen for SPRITE_EVICT_FRAMES — so memory
        // scales with visible area instead of total node count.
        const laidNodesLive = laidNodesRef.current;
        if (
          laidNodesLive.length > 0 &&
          spriteCtxRef.current &&
          scene.nodeContainer
        ) {
          const nodeContainer = scene.nodeContainer;
          const spriteCtx = spriteCtxRef.current;
          const lod = currentLodRef.current;
          // SDF text mode: full-LOD textures carry only flat-color
          // chrome, so they never need the zoom-proportional resolution
          // ladder — a fixed DPR both kills bucket-crossing rebuilds
          // and keeps the cache key stable.
          const dpr =
            SDF_TEXT_ENABLED && lod === "full"
              ? SDF_CHROME_DPR
              : spriteDprForLod(lod, v.k);
          spriteDprRef.current = dpr;

          const prev = lastSpriteSweepViewRef.current;
          const viewMoved =
            Math.abs(v.x - prev.x) > 40 ||
            Math.abs(v.y - prev.y) > 40 ||
            Math.abs(v.k - prev.k) / Math.max(0.0001, prev.k) > 0.05 ||
            lod !== prev.lod;

          // The sweep iterates while the view actually moved OR while a
          // focus-jump is still draining its create/build queues. The
          // second clause is the long-distance navigation fix: on
          // frame 1 of a focus pan the sweep discovers in-view nodes
          // with no sprite yet and pushes them to the create queue.
          // On frame 2+ the create queue produces sprites with
          // placeholder textures. Without re-entering the sweep,
          // those sprites would never be queued for full-LOD texture
          // build, so they'd stay on the kind placeholder forever.
          if (viewMoved || focusJumpPendingRef.current) {
            if (viewMoved) {
              lastSpriteSweepViewRef.current = { x: v.x, y: v.y, k: v.k, lod };
              lastViewChangeAtRef.current = performance.now();
            }
            ticksSweptRef.current++;

            const vpMinX = -v.x / v.k - SPRITE_VIEW_PADDING;
            const vpMinY = -v.y / v.k - SPRITE_VIEW_PADDING;
            const vpMaxX = (sw - v.x) / v.k + SPRITE_VIEW_PADDING;
            const vpMaxY = (sh - v.y) / v.k + SPRITE_VIEW_PADDING;

            let queue = spriteBuildQueueRef.current;
            if (queue && (queue.lod !== lod || queue.dpr !== dpr)) {
              queue = null;
              spriteBuildQueueRef.current = null;
            }
            const queuedIds = queue
              ? new Set(queue.nodes.map((n) => n.id))
              : new Set<string>();

            // Gather in-view candidates via the node tile index so a
            // pan/zoom frame only examines nodes whose tiles intersect
            // the viewport instead of every laid node. When the
            // viewport spans more cells than the index holds (fully
            // zoomed out) a full scan is cheaper — fall back to it.
            // Sprites are still created lazily per sweep; upfront
            // allocation for all 1,400 nodes stalled the Pixi renderer
            // hard enough to crash the tab.
            const nodeIndex = nodeTileIndexRef.current;
            const tminC = Math.floor(vpMinX / TILE_SIZE);
            const tmaxC = Math.floor(vpMaxX / TILE_SIZE);
            const tminR = Math.floor(vpMinY / TILE_SIZE);
            const tmaxR = Math.floor(vpMaxY / TILE_SIZE);
            const cellCount = (tmaxC - tminC + 1) * (tmaxR - tminR + 1);
            let candidates: LaidNode[];
            if (cellCount < nodeIndex.size) {
              candidates = [];
              const seenIds = new Set<string>();
              for (let c = tminC; c <= tmaxC; c++) {
                for (let r = tminR; r <= tmaxR; r++) {
                  const bucket = nodeIndex.get(`${c},${r}`);
                  if (!bucket) continue;
                  for (const node of bucket) {
                    if (!seenIds.has(node.id)) {
                      seenIds.add(node.id);
                      candidates.push(node);
                    }
                  }
                }
              }
            } else {
              candidates = laidNodesLive;
            }

            const idsToEvict = new Set<string>();
            for (const node of candidates) {
              const inView = !(
                node.cx + node.w / 2 < vpMinX ||
                node.cx - node.w / 2 > vpMaxX ||
                node.cy + node.h / 2 < vpMinY ||
                node.cy - node.h / 2 > vpMaxY
              );
              const id = node.id;
              const sprite = nodeSpritesRef.current.get(id);

              if (inView) {
                if (!sprite) {
                  // Defer to the progressive create queue. Allocating
                  // here would synchronously spawn N sprites when the
                  // viewport first covers a big grid.
                  if (!spriteCreateQueueRef.current) {
                    spriteCreateQueueRef.current = [];
                  }
                  spriteCreateQueueRef.current.push(node);
                  spriteLastSeenFrameRef.current.set(
                    id,
                    frameCounterRef.current,
                  );
                  continue;
                }
                spriteLastSeenFrameRef.current.set(id, frameCounterRef.current);
                // Defensive: a sprite must never sit on a destroyed
                // texture (rendering one crashes Pixi with a null
                // source). Should be unreachable now that the texture
                // purge detaches sprites first, but a placeholder swap
                // is cheap insurance against future purge paths.
                if (sprite.texture.destroyed) {
                  const kindTex = kindTextureCacheRef.current.get(node.data.kind);
                  sprite.texture = kindTex ?? Texture.WHITE;
                  if (kindTex) {
                    sprite.tint = 0xffffff;
                    sprite.leftWidth = PLACEHOLDER_CORNER;
                    sprite.topHeight = PLACEHOLDER_HEADER_H;
                    sprite.rightWidth = PLACEHOLDER_CORNER;
                    sprite.bottomHeight = PLACEHOLDER_CORNER;
                  }
                }
                // Endpoints of the currently-focused edge AND the
                // currently-focused node (the navigation target) are
                // always rendered at full LOD so the user can read
                // type names + fields even after zooming out, and so
                // a click-to-navigate doesn't briefly leave the
                // target on its low-LOD placeholder while the build
                // queue waits for motion-settle. The texture is
                // built synchronously here (cheap — at most 3
                // sprites) so we don't have to thread a mixed-LOD
                // build queue.
                const focusedE = focusedEdgeRef.current;
                const focusedNodeId = focusIdRef.current;
                const forceFull =
                  (!!focusedE &&
                    (focusedE.sourceId === id || focusedE.targetId === id)) ||
                  focusedNodeId === id;
                // Non-full LOD: show the shared per-kind placeholder.
                // Six uploads total, reused across every sprite of the
                // same kind — cheap and gives a proper card silhouette
                // instead of the old solid tinted rectangle.
                if (lod !== "full" && !forceFull) {
                  // Keep the (still cached) text mesh but hide it —
                  // bar/chrome placeholders carry their own fake text.
                  const tm = nodeTextMeshesRef.current.get(id);
                  if (tm && tm.visible) tm.visible = false;
                  const kindTex = kindTextureCacheRef.current.get(
                    node.data.kind,
                  );
                  if (kindTex && sprite.texture !== kindTex) {
                    sprite.texture = kindTex;
                    sprite.tint = 0xffffff;
                    sprite.leftWidth = PLACEHOLDER_CORNER;
                    sprite.topHeight = PLACEHOLDER_HEADER_H;
                    sprite.rightWidth = PLACEHOLDER_CORNER;
                    sprite.bottomHeight = PLACEHOLDER_CORNER;
                  }
                  continue;
                }
                const effLod: SpriteLOD = forceFull ? "full" : lod;
                const effDpr = forceFull
                  ? SDF_TEXT_ENABLED
                    ? SDF_CHROME_DPR
                    : spriteDprForLod("full", v.k)
                  : dpr;
                const key = `${id}:${effLod}:${effDpr}`;
                if (SDF_TEXT_ENABLED) {
                  const tm = nodeTextMeshesRef.current.get(id);
                  if (tm && !tm.visible) tm.visible = true;
                }
                const cachedTex = textureCacheRef.current.get(key);
                if (cachedTex) {
                  if (sprite.texture !== cachedTex) {
                    sprite.texture = cachedTex;
                    sprite.tint = 0xffffff;
                    sprite.leftWidth = 0;
                    sprite.topHeight = 0;
                    sprite.rightWidth = 0;
                    sprite.bottomHeight = 0;
                  }
                } else if (forceFull) {
                  // Synchronous build for focused endpoints — bypasses
                  // the motion-settle gate and the build queue so the
                  // selection feels immediate.
                  syncBuildCountRef.current++;
                  const drawDpr = fitDprToMaxTexture(node.w, node.h, effDpr);
                  const pw = Math.ceil(node.w * drawDpr);
                  const ph = Math.ceil(node.h * drawDpr);
                  const can = document.createElement("canvas");
                  can.width = pw;
                  can.height = ph;
                  const c2d = can.getContext("2d");
                  if (c2d) {
                    c2d.setTransform(drawDpr, 0, 0, drawDpr, 0, 0);
                    const sink: TextRun[] | null = SDF_TEXT_ENABLED ? [] : null;
                    drawNodeSprite(c2d, node, spriteCtx, "full", sink);
                    if (sink) {
                      ensureNodeTextMeshRef.current(node, sink);
                      flushSdfAtlas();
                    }
                    const tex = Texture.from(can);
                    textureCacheRef.current.set(key, tex);
                    sprite.texture = tex;
                    sprite.tint = 0xffffff;
                    sprite.leftWidth = 0;
                    sprite.topHeight = 0;
                    sprite.rightWidth = 0;
                    sprite.bottomHeight = 0;
                  }
                } else if (!queuedIds.has(id)) {
                  if (!spriteBuildQueueRef.current) {
                    spriteBuildQueueRef.current = {
                      nodes: [],
                      lod,
                      dpr,
                      spriteCtx,
                      dimNodeIds: new Set<string>(),
                    };
                  }
                  spriteBuildQueueRef.current.nodes.push(node);
                  queuedIds.add(id);
                }
              }
            }

            // Eviction pass over the sprite map. It can't live in the
            // candidate loop above — a sprite whose tile left the
            // viewport never appears in `candidates`. Sprites visited
            // in-view this frame have lastSeen == current frame.
            for (const [id, sprite] of nodeSpritesRef.current) {
              const last = spriteLastSeenFrameRef.current.get(id) ?? 0;
              if (last === frameCounterRef.current) continue;
              if (frameCounterRef.current - last > SPRITE_EVICT_FRAMES) {
                idsToEvict.add(id);
                nodeContainer.removeChild(sprite);
                sprite.destroy();
                nodeSpritesRef.current.delete(id);
                spriteLastSeenFrameRef.current.delete(id);
                destroyNodeTextMeshRef.current(id);
              }
            }

            // Second pass: single scan over the texture cache — drop
            // keys whose id prefix is in the evict set. Safe to delete
            // during Map iteration; V8 skips removed unvisited entries.
            if (idsToEvict.size > 0) {
              for (const key of textureCacheRef.current.keys()) {
                const sep = key.indexOf(":");
                if (sep < 0) continue;
                const ownerId = key.slice(0, sep);
                if (idsToEvict.has(ownerId)) {
                  const tex = textureCacheRef.current.get(key);
                  if (tex) tex.destroy(true);
                  textureCacheRef.current.delete(key);
                }
              }
            }
          }
        }

        // Progressive sprite building — drain the queue a few nodes
        // per frame (4ms budget). Gated on view stability: while the
        // user is actively panning or zooming, a steady stream of
        // `Texture.from(canvas)` uploads crashes mobile GPU drivers,
        // so we wait for MOTION_SETTLE_MS of no significant view
        // change before resuming. Sprites stay on their tint
        // placeholder until then.
        //
        // Exception: when a one-shot focus jump just happened (an
        // explicit navigation, not a continuous pan), bypass the
        // gate and give a larger per-frame budget so the user sees
        // the newly-focused area at full LOD within ~1 frame instead
        // of waiting out the 150 ms settle. The flag stays set until
        // the queue drains, so even spillover sprites in subsequent
        // frames don't pay the gate.
        const buildQ = spriteBuildQueueRef.current;
        const focusJumping = focusJumpPendingRef.current;
        const motionStable =
          focusJumping ||
          performance.now() - lastViewChangeAtRef.current >= MOTION_SETTLE_MS;
        if (buildQ && buildQ.nodes.length > 0 && motionStable) {
          // Focus jumps keep their deliberate one-frame 30 ms burst so
          // navigation feels immediate; steady-state builds share the
          // frame work deadline and are additionally capped by upload
          // volume (texels) so one batch of huge enum nodes can't
          // queue tens of MB of texImage2D in a single frame.
          const deadline = focusJumping ? performance.now() + 30 : workDeadline;
          const lodCap = maxTextureCacheFor(buildQ.lod, buildQ.dpr);
          let texelsThisFrame = 0;
          while (buildQ.nodes.length > 0 && performance.now() < deadline) {
            if (textureCacheRef.current.size >= lodCap) break;
            if (!focusJumping && texelsThisFrame >= texelBudget) break;
            const n = buildQ.nodes.pop()!;
            const key = `${n.id}:${buildQ.lod}:${buildQ.dpr}`;
            if (textureCacheRef.current.has(key)) continue;

            const drawDpr = fitDprToMaxTexture(n.w, n.h, buildQ.dpr);
            const pw = Math.ceil(n.w * drawDpr);
            const ph = Math.ceil(n.h * drawDpr);
            texelsThisFrame += pw * ph;
            const can = document.createElement("canvas");
            can.width = pw;
            can.height = ph;
            const c2d = can.getContext("2d");
            if (c2d) {
              c2d.setTransform(drawDpr, 0, 0, drawDpr, 0, 0);
              const sink: TextRun[] | null =
                SDF_TEXT_ENABLED && buildQ.lod === "full" ? [] : null;
              drawNodeSprite(c2d, n, buildQ.spriteCtx, buildQ.lod, sink);
              if (sink) ensureNodeTextMeshRef.current(n, sink);
              const tex = Texture.from(can);
              textureCacheRef.current.set(key, tex);
              const spr = nodeSpritesRef.current.get(n.id);
              if (spr) {
                spr.texture = tex;
                spr.tint = 0xffffff;
                spr.leftWidth = 0;
                spr.topHeight = 0;
                spr.rightWidth = 0;
                spr.bottomHeight = 0;
              }
            }
          }
          // One GPU upload for every glyph baked by this frame's
          // builds (no-op when the atlas is clean).
          if (SDF_TEXT_ENABLED) flushSdfAtlas();
          if (buildQ.nodes.length === 0) {
            // Drain complete: every sprite currently in view has a
            // texture for `buildQ.lod` at `buildQ.dpr`. Any cached
            // texture keyed to a different LOD or DPR bucket can be
            // released to cap GPU memory during LOD/zoom zigzags.
            //
            // CAUTION: "different key" does NOT mean "unreferenced" —
            // off-screen sprites keep their last texture until the
            // evict window (SPRITE_EVICT_FRAMES) passes, and Pixi
            // renders a destroyed texture as a null source, crashing
            // with "Cannot read properties of null (reading
            // 'alphaMode')". Detach any sprite still holding a purged
            // texture back to its kind placeholder before destroying.
            const keepSuffix = `:${buildQ.lod}:${buildQ.dpr}`;
            const toDelete: string[] = [];
            for (const key of textureCacheRef.current.keys()) {
              if (!key.endsWith(keepSuffix)) toDelete.push(key);
            }
            for (const key of toDelete) {
              const tex = textureCacheRef.current.get(key);
              if (tex) {
                const ownerId = key.slice(0, key.indexOf(":"));
                const spr = nodeSpritesRef.current.get(ownerId);
                if (spr && spr.texture === tex) {
                  const kind = nodeByIdRef.current.get(ownerId)?.data.kind;
                  const kindTex = kind
                    ? kindTextureCacheRef.current.get(kind)
                    : undefined;
                  spr.texture = kindTex ?? Texture.WHITE;
                  if (kindTex) {
                    spr.tint = 0xffffff;
                    spr.leftWidth = PLACEHOLDER_CORNER;
                    spr.topHeight = PLACEHOLDER_HEADER_H;
                    spr.rightWidth = PLACEHOLDER_CORNER;
                    spr.bottomHeight = PLACEHOLDER_CORNER;
                  }
                }
                tex.destroy(true);
              }
              textureCacheRef.current.delete(key);
              // A purged full-LOD texture takes its text mesh with it
              // so the pair stays in lockstep (both rebuild together
              // on the next full-LOD sweep).
              if (key.includes(":full:")) {
                destroyNodeTextMeshRef.current(key.slice(0, key.indexOf(":")));
              }
            }
            spriteBuildQueueRef.current = null;
            // Only clear the focus-jump flag once sprite *creation*
            // is also done — long-distance jumps queue tens of new
            // sprites that take several frames to materialize, and
            // each must re-enter the sweep to get its full-LOD
            // texture queued. Clearing the flag too early would lock
            // the late arrivals on their placeholder textures.
            const createPending =
              !!spriteCreateQueueRef.current &&
              spriteCreateQueueRef.current.length > 0;
            if (!createPending) focusJumpPendingRef.current = false;
          }
        } else if (!buildQ && focusJumpPendingRef.current) {
          // Nothing currently queued for build. Clear the flag only
          // when sprite creation is also idle; otherwise the sprites
          // still being created on subsequent frames will need to
          // re-enter the sweep to queue their textures.
          const createPending =
            !!spriteCreateQueueRef.current &&
            spriteCreateQueueRef.current.length > 0;
          if (!createPending) focusJumpPendingRef.current = false;
        }

        // FPS sampling
        const now = performance.now();
        fpsTimes.push(now);
        let lo = 0;
        while (lo < fpsTimes.length && now - fpsTimes[lo]! > 1000) lo++;
        if (lo > 0) fpsTimes.splice(0, lo);
        if (now - lastFpsSampleAt >= 200) {
          lastFpsSampleAt = now;
          const fps = fpsTimes.length;
          if (fpsTextRef.current) fpsTextRef.current.textContent = `${fps} fps`;
          const hist = fpsHistoryRef.current;
          hist.push(fps);
          if (hist.length > 60) hist.shift();
          // Draw chart directly — no React re-render needed.
          const cc = chartCanvasRef.current;
          if (cc) {
            const cw = cc.width;
            const ch = cc.height;
            const cctx = cc.getContext("2d");
            if (cctx) {
              cctx.clearRect(0, 0, cw, ch);
              // Scale the chart to the observed peak so a 120 Hz
              // display isn't clipped at the old 60-ish ceiling.
              let peak = 65;
              for (const f of hist) if (f > peak) peak = f;
              const maxFps = peak + 5;
              const barW = cw / hist.length;
              for (let i = 0; i < hist.length; i++) {
                const v = hist[i]!;
                const bh = Math.max(1, (v / maxFps) * ch);
                // "Low" is relative to the observed peak (≈ display
                // refresh) so dips read correctly on 120 Hz too.
                const isLow = v < peak * 0.5;
                cctx.fillStyle = isLow ? "rgba(248,113,113,0.7)" : "rgba(148,163,184,0.35)";
                cctx.fillRect(i * barW, ch - bh, Math.max(1, barW - 1), bh);
              }
            }
          }
        }

        // Idle-stop bookkeeping — see wake() above.
        const anyWork =
          viewChanged ||
          tileWork ||
          animActive ||
          focusJumpPendingRef.current ||
          (spriteCreateQueueRef.current?.length ?? 0) > 0 ||
          (spriteBuildQueueRef.current?.nodes.length ?? 0) > 0;
        if (anyWork) idleFrames = 0;
        else if (++idleFrames >= IDLE_STOP_FRAMES) app.ticker.stop();
      });
    });

    return () => {
      destroyed = true;
      const app = appRef.current;
      if (app) {
        app.destroy(true, { children: true, texture: true });
        appRef.current = null;
      }
      sceneRef.current = {
        gridTiling: null,
        world: null,
        edgeTileContainer: null,
            activeEdgeContainer: null,
            focusEdgeGraphics: null,
        hoverEdgeGraphics: null,
        nodeContainer: null,
        textContainer: null,
        investigateOverlay: null,
        pinFieldGraphics: null,
        hoverGraphics: null,
        focusGraphics: null,
      };
      nodeTextMeshesRef.current.clear();
      // Explicit tile cache teardown. Pixi destroys Graphics children
      // via app.destroy, but holding stale references in the ref would
      // leak when the component remounts.
      edgeTilesRef.current.clear();
      for (const tex of kindTextureCacheRef.current.values()) {
        tex.destroy(true);
      }
      kindTextureCacheRef.current.clear();
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Keep focusId accessible in the ticker without re-init
  const focusIdRef = useRef(focusId ?? null);
  focusIdRef.current = focusId ?? null;

  // Timestamp of the last focus change — bounds the focus-ring ripple
  // animation so an idle canvas with a parked focus can stop rendering.
  const focusChangedAtRef = useRef(0);
  useEffect(() => {
    focusChangedAtRef.current = performance.now();
  }, [focusId]);

  // Wake the render loop on every React render. Any state- or prop-
  // driven scene mutation (focus, dim, pin, investigate, layout,
  // theme, resize…) flows through a render, so this single effect
  // covers them all; only the raw pointer/wheel/touch handlers that
  // mutate viewRef without rendering need explicit wake() calls.
  useEffect(() => {
    wakeRef.current();
  });

  // Keep nodeById accessible in the ticker
  const nodeByIdRef = useRef(nodeById);
  nodeByIdRef.current = nodeById;

  // Keep the node spatial index accessible in the ticker's sweep
  const nodeTileIndexRef = useRef(nodeTileIndex);
  nodeTileIndexRef.current = nodeTileIndex;

  // Keep `laidNodes` accessible in the ticker without re-adding the
  // ticker callback each render — the ticker is registered once and
  // captures its surrounding closure's `laidNodes` value (initially
  // empty), so state updates need this ref to be seen.
  const laidNodesRef = useRef(laidNodes);
  laidNodesRef.current = laidNodes;

  // Diagnostic counters for the e2e test.
  const spriteResetCountRef = useRef(0);
  const ticksSweptRef = useRef(0);
  const syncBuildCountRef = useRef(0);

  // Debug introspection hook — lets the Playwright e2e test verify
  // post-navigation LOD/texture state without screen-scraping pixels.
  // Read-only; no production code path consumes it.
  useEffect(() => {
    if (typeof window === "undefined") return;
    const w = window as unknown as { __gqlCanvas?: unknown };
    w.__gqlCanvas = {
      getLod: () => currentLodRef.current,
      getView: () => ({ ...viewRef.current }),
      getTextureKeys: () => [...textureCacheRef.current.keys()],
      getSpriteIds: () => [...nodeSpritesRef.current.keys()],
      getLaidNodeCount: () => laidNodesRef.current.length,
      getFocusId: () => focusIdRef.current,
      isFocusJumpPending: () => focusJumpPendingRef.current,
      getSpriteResetCount: () => spriteResetCountRef.current,
      getTicksSwept: () => ticksSweptRef.current,
      getSyncBuildCount: () => syncBuildCountRef.current,
      /** Test-only: SDF text mesh stats per node — chunk count, quads
       *  per chunk, and visibility — to distinguish build-side from
       *  draw-side text loss. */
      /** Test-only: run the canvas field hit-test at a world point.
       *  Via ref — the effect's closure would otherwise capture a
       *  pre-layout hitTestField with an empty node index. */
      hitTestFieldAt: (wx: number, wy: number) => hitTestFieldRef.current(wx, wy),
      getTextMeshInfo: () =>
        [...nodeTextMeshesRef.current.entries()].map(([id, tm]) => ({
          id,
          visible: tm.visible,
          alpha: tm.alpha,
          x: tm.x,
          y: tm.y,
          chunks: tm.children.map((c) => {
            const geom = (c as Mesh).geometry;
            return {
              indices: geom.indexBuffer?.data?.length ?? 0,
              positions: geom.getAttribute("aPosition")?.buffer?.data?.length ?? 0,
              visible: c.visible,
            };
          }),
        })),
      /** Test-only: drive a navigation through the same code path
       *  that a canvas return-type click or a tree-panel field
       *  click uses. */
      navigate: (id: string) => onNavigateRef.current?.(id),
      /** Test-only: per-node row + header heights, so an e2e test
       *  can verify that toggling "Show descriptions" actually
       *  re-laid the graph with the larger row sizes. */
      getNodeDimensions: () =>
        laidNodesRef.current.map((n) => ({
          id: n.id,
          rowH: n.rowH,
          headerH: n.headerH,
          w: n.w,
          h: n.h,
          cx: n.cx,
          cy: n.cy,
        })),
      getInViewNodeIds: () => {
        const v = viewRef.current;
        const sw = size.w;
        const sh = size.h;
        const vpMinX = -v.x / v.k - SPRITE_VIEW_PADDING;
        const vpMinY = -v.y / v.k - SPRITE_VIEW_PADDING;
        const vpMaxX = (sw - v.x) / v.k + SPRITE_VIEW_PADDING;
        const vpMaxY = (sh - v.y) / v.k + SPRITE_VIEW_PADDING;
        return laidNodesRef.current
          .filter(
            (n) =>
              !(
                n.cx + n.w / 2 < vpMinX ||
                n.cx - n.w / 2 > vpMaxX ||
                n.cy + n.h / 2 < vpMinY ||
                n.cy - n.h / 2 > vpMaxY
              ),
          )
          .map((n) => n.id);
      },
    };
    return () => {
      const w2 = window as unknown as { __gqlCanvas?: unknown };
      w2.__gqlCanvas = undefined;
    };
  }, [size.w, size.h]);

  // Resize Pixi renderer
  useEffect(() => {
    const app = appRef.current;
    if (!app || size.w <= 1 || size.h <= 1) return;
    app.renderer.resize(size.w, size.h);
    const scene = sceneRef.current;
    if (scene.gridTiling) {
      scene.gridTiling.width = size.w;
      scene.gridTiling.height = size.h;
    }
  }, [size]);

  // Ticker-accessible foreground color, refreshed when the theme
  // changes (and once the app is ready, since the .dark class may
  // toggle after the first effect pass).
  const fgHexRef = useRef(0x0f172a);
  useEffect(() => {
    fgHexRef.current = cssColorToHex(getComputedCssVar("--foreground", "#0f172a"));
  }, [themeResolved, appReady]);

  // Rebuild dot grid texture when theme changes
  useEffect(() => {
    const scene = sceneRef.current;
    if (!scene.gridTiling) return;
    const mutedFg = getComputedCssVar("--muted-foreground", "#64748b");
    const mutedHex = cssColorToHex(mutedFg);
    const tex = buildDotGridTexture(mutedHex, 0.18);
    scene.gridTiling.texture = tex;
    scene.gridTiling.tileScale.set(1);
  }, [themeResolved]);

  // Keep a reference to the latest edgeGroups (with colors/alphaScales) so
  // the ticker can rebuild a tile's Graphics on demand without closing
  // over a stale effect value.
  const edgeGroupsRef = useRef(edgeGroups);
  edgeGroupsRef.current = edgeGroups;

  // Rebuild tile assignments when the edge set changes (new layout /
  // schema). Focus-dim state is deliberately NOT a dependency — dim is
  // applied via container alpha and the active overlay effect below,
  // so navigating never destroys and re-tessellates tile Graphics.
  useEffect(() => {
    const scene = sceneRef.current;
    if (!scene.edgeTileContainer) return;

    // Drop old tile Graphics. New grouping means existing vertex
    // buffers are invalid.
    for (const tile of edgeTilesRef.current.values()) {
      for (const g of tile.edgeBatches) g.destroy({ children: true });
    }
    edgeTilesRef.current.clear();
    scene.edgeTileContainer.removeChildren();

    const lod = currentLodRef.current;
    if (lod === "chrome") return;

    const assign = (edge: LaidEdge, gi: number) => {
      const minCol = Math.floor(edge.bbox.minX / TILE_SIZE);
      const maxCol = Math.floor(edge.bbox.maxX / TILE_SIZE);
      const minRow = Math.floor(edge.bbox.minY / TILE_SIZE);
      const maxRow = Math.floor(edge.bbox.maxY / TILE_SIZE);
      for (let c = minCol; c <= maxCol; c++) {
        for (let r = minRow; r <= maxRow; r++) {
          const key = `${c},${r}`;
          let tile = edgeTilesRef.current.get(key);
          if (!tile) {
            tile = {
              key,
              col: c,
              row: r,
              groupLists: EDGE_GROUP_DEFS.map(() => []),
              edgeBatches: [],
              builtBatches: 0,
              totalBatches: -1,
              lastSeenFrame: 0,
            };
            edgeTilesRef.current.set(key, tile);
          }
          tile.groupLists[gi]!.push(edge);
        }
      }
    };

    edgeBuckets.forEach((list, gi) => {
      for (const e of list) assign(e, gi);
    });
    // Deliberately omit `lodTick`: rebuilding tile assignments (and
    // destroying all live tile Graphics) on every LOD crossing was the
    // real source of the boundary frame drop — many tiles would all
    // lazy-rebuild on the next frame. The tile structure is
    // LOD-independent; only the container visibility toggles below
    // react to LOD changes.
  }, [edgeBuckets]);

  // Focus-dim application. Dimming fades the *entire* tile containers
  // via alpha (no geometry invalidation), then redraws the handful of
  // active edges at full alpha into the overlay containers on top.
  // This replaces the old approach of partitioning every tile's edge
  // list into dim/active and re-tessellating all tiles per focus
  // change.
  useEffect(() => {
    const scene = sceneRef.current;
    if (!scene.edgeTileContainer || !scene.activeEdgeContainer) return;

    const dimming = edgeGroups.groups.some((g) => g.dim.length > 0);
    scene.edgeTileContainer.alpha = dimming ? DIM_ALPHA : 1;

    for (const c of scene.activeEdgeContainer.removeChildren()) c.destroy({ children: true });
    if (!dimming) return;

    for (const g of edgeGroups.groups) {
      for (let i = 0; i < g.active.length; i += EDGES_PER_BATCH) {
        const built = buildEdgeBatchMesh(
          g.active.slice(i, i + EDGES_PER_BATCH),
          g,
          1,
        );
        scene.activeEdgeContainer.addChild(built);
      }
    }
  }, [edgeGroups]);

  // chrome LOD hides edges entirely. Toggle container visibility on
  // the root tile containers — no per-tile destroy/rebuild needed.
  useEffect(() => {
    const scene = sceneRef.current;
    if (!scene.edgeTileContainer) return;
    const show = currentLodRef.current !== "chrome";
    scene.edgeTileContainer.visible = show;
    if (scene.activeEdgeContainer) scene.activeEdgeContainer.visible = show;
  }, [lodTick]);

  // Effect A: sprite lifecycle reset. Runs only when the node set or
  // theme changes. Destroys any pre-existing textures and sprites so
  // the ticker's viewport sweep can rebuild from scratch — crucially,
  // we do NOT upfront-allocate the 1,400+ `new Sprite` + `addChild`
  // pairs anymore. On a big schema that synchronous loop would stall
  // the Pixi renderer hard enough to crash the tab ("GPU stall due to
  // ReadPixels"). Sprites are now created lazily per viewport sweep
  // (see ticker below), so mount stays cheap regardless of node count.
  useEffect(() => {
    const scene = sceneRef.current;
    if (!scene.nodeContainer) return;

    for (const tex of textureCacheRef.current.values()) tex.destroy(true);
    textureCacheRef.current.clear();
    spriteDprRef.current = 0;

    for (const spr of nodeSpritesRef.current.values()) spr.destroy();
    nodeSpritesRef.current.clear();
    scene.nodeContainer.removeChildren();

    for (const tm of nodeTextMeshesRef.current.values()) {
      tm.destroy({ children: true });
    }
    nodeTextMeshesRef.current.clear();
    scene.textContainer?.removeChildren();

    const cardColor = getComputedCssVar("--card", "#ffffff");
    const fgColor = getComputedCssVar("--foreground", "#0f172a");
    const mutedFg = getComputedCssVar("--muted-foreground", "#64748b");
    const spriteCtx: SpriteCtx = { cardColor, fgColor, mutedFg };
    spriteCtxRef.current = spriteCtx;

    spriteLastSeenFrameRef.current.clear();
    spriteBuildQueueRef.current = null;
    spriteCreateQueueRef.current = null;
    spriteResetCountRef.current++;
  }, [laidNodes, themeResolved]);

  // Dim/undim node sprites when focus changes — lightweight alpha-only
  // update, no texture rebuild. Separated from the sprite build effect
  // so focus changes don't destroy+recreate all sprites (which caused
  // a visible flash as placeholders briefly appeared).
  useEffect(() => {
    const { dimNodeIds } = edgeGroups;
    const DIM = 0.1;
    for (const [id, sprite] of nodeSpritesRef.current) {
      sprite.alpha = dimNodeIds.has(id) ? DIM : 1;
    }
    for (const [id, tm] of nodeTextMeshesRef.current) {
      tm.alpha = dimNodeIds.has(id) ? DIM : 1;
    }
  }, [edgeGroups]);

  // When a field is pinned (tree-panel click, Until-panel, or canvas
  // field click), don't try to fit the whole owner type — a big type like
  // Mutation never comes into view usefully. Instead just bring the
  // *clicked field's row* to the centre of the canvas at a readable zoom.
  // Deterministic — derived only from the node bounds and viewport, never
  // the current zoom — so repeated clicks land identically.
  useEffect(() => {
    if (!pinnedField) return;
    const n = nodeById.get(pinnedField.typeId);
    if (!n) return;
    const margin = 48;
    const availW = Math.max(1, size.w - margin * 2);
    // Readable zoom from the node width, clamped to a legible range.
    const targetK = Math.min(Math.max(availW / n.w, 0.7), 1.3);

    // World Y of the clicked row's centre. Match the row by NAME against
    // the canvas node's own field/value list — the tree's fieldIndex can
    // diverge from the canvas node's array (primitive-field filtering,
    // Relay unwrapping, ordering), which would otherwise drop us onto the
    // node centre instead of the actual field. Falls back to the given
    // index, then the node centre.
    const rows: { name: string }[] =
      n.data.kind === "Enum" ? (n.data.values ?? []) : (n.data.fields ?? []);
    let rowIdx = rows.findIndex((r) => r.name === pinnedField.fieldName);
    if (rowIdx < 0 && pinnedField.fieldIndex >= 0 && pinnedField.fieldIndex < rows.length) {
      rowIdx = pinnedField.fieldIndex;
    }
    let rowY = n.cy;
    if (rowIdx >= 0) {
      const bodyTop = n.headerH + TOP_BODY_PAD - 2;
      rowY = n.cy - n.h / 2 + bodyTop + rowIdx * n.rowH + n.rowH / 2;
    }

    viewRef.current = {
      k: targetK,
      x: size.w / 2 - n.cx * targetK,
      y: size.h / 2 - rowY * targetK,
    };
    // The view just jumped; refresh the LOD immediately and bypass the
    // motion-settle gate so the framed node's text renders at once
    // rather than sitting on its low-detail placeholder.
    const newLod = computeLOD(targetK, currentLodRef.current);
    if (newLod !== currentLodRef.current) {
      currentLodRef.current = newLod;
      setLodTick((t) => t + 1);
    }
    focusJumpPendingRef.current = true;
    wakeRef.current();
    // Intentionally NOT keyed on size: re-frame only on an actual pin
    // change, not while the canvas width animates during a sidebar
    // collapse/expand — otherwise the view would drift and the fixed
    // overlays would appear to move. `size` is read fresh from the
    // closure whenever a real pin change re-runs this.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pinnedField, nodeById]);

  // Record every field pin (tree-panel click, Until-panel selection, or
  // canvas field click) in the Recent history, alongside node/edge
  // clicks — clicking the entry re-frames and re-highlights the field.
  useEffect(() => {
    if (!pinnedField) return;
    const n = nodeById.get(pinnedField.typeId);
    if (!n) return;
    pushHistory({
      kind: "field",
      id: `field:${pinnedField.typeId}.${pinnedField.fieldName}`,
      typeId: pinnedField.typeId,
      typeName: n.data.name,
      fieldName: pinnedField.fieldName,
      fieldIndex: pinnedField.fieldIndex,
      nodeKind: n.data.kind,
      ts: Date.now(),
    });
    // pushHistory is a stable closure over setClickHistory; excluded to
    // avoid re-pushing on unrelated re-renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pinnedField, nodeById]);

  // Pinned-field highlight — draws a persistent orange ring around
  // the specific field row a user clicked in the tree panel. Reads
  // pinnedField from schema-context. Re-runs whenever the pin or
  // layout (laidNodes) changes.
  useEffect(() => {
    const g = sceneRef.current.pinFieldGraphics;
    if (!g) return;
    g.clear();
    if (!pinnedField) return;
    const n = nodeById.get(pinnedField.typeId);
    if (!n) return;
    const bodyTop = n.headerH + TOP_BODY_PAD - 2;
    const nodeLeft = n.cx - n.w / 2;
    const nodeTop = n.cy - n.h / 2;
    // Enum values are laid out on the same row grid as object fields.
    // Match by name against the canvas node's own list (the tree's
    // fieldIndex can diverge from it), falling back to the given index.
    const rows: { name: string }[] =
      n.data.kind === "Enum" ? (n.data.values ?? []) : (n.data.fields ?? []);
    let rowIdx = rows.findIndex((r) => r.name === pinnedField.fieldName);
    if (rowIdx < 0 && pinnedField.fieldIndex >= 0 && pinnedField.fieldIndex < rows.length) {
      rowIdx = pinnedField.fieldIndex;
    }
    if (rowIdx < 0) return;
    const y = nodeTop + bodyTop + rowIdx * n.rowH;
    const pad = 2;
    // A soft fill under a bright ring so the pinned row reads clearly at
    // a glance, even against the node's own colored body.
    g.roundRect(nodeLeft + pad, y - pad, n.w - pad * 2, n.rowH + pad * 2, 4);
    g.fill({ color: 0xf97316, alpha: 0.18 });
    g.roundRect(nodeLeft + pad, y - pad, n.w - pad * 2, n.rowH + pad * 2, 4);
    g.stroke({ width: 2, color: 0xf97316, alpha: 0.95 });
  }, [pinnedField, nodeById]);

  // Redraw the investigate-mode overlay whenever the match set or
  // node layout changes. Two layers of highlight:
  //   1. Per-row orange fill on each field/value whose description
  //      is missing (so the user can spot specific rows).
  //   2. Outline around the whole node when its own description
  //      (not just rows) is missing.
  useEffect(() => {
    const g = sceneRef.current.investigateOverlay;
    if (!g) return;
    g.clear();
    if (!investigateMatch) return;

    // Row stripes first (drawn underneath the outline if both apply
    // to the same node).
    for (const n of laidNodes) {
      const rows = investigateMatch.rowsByNode.get(n.id);
      if (!rows) continue;
      const bodyTop = n.headerH + TOP_BODY_PAD - 2;
      const left = n.cx - n.w / 2 + 4;
      const top = n.cy - n.h / 2;
      const width = n.w - 8;
      for (const rowIdx of rows) {
        const y = top + bodyTop + rowIdx * n.rowH;
        g.roundRect(left, y, width, n.rowH, 3);
      }
    }
    g.fill({ color: 0xf97316, alpha: 0.22 });

    // Outline pass on top.
    g.beginPath();
    const PAD = 4;
    for (const n of laidNodes) {
      if (!investigateMatch.nodeOutline.has(n.id)) continue;
      g.roundRect(
        n.cx - n.w / 2 - PAD,
        n.cy - n.h / 2 - PAD,
        n.w + PAD * 2,
        n.h + PAD * 2,
        8,
      );
    }
    g.stroke({ width: 3, color: 0xf97316, alpha: 0.95 });
  }, [investigateMatch, laidNodes]);

  // Redraw the hovered-edge highlight whenever the hovered edge
  // changes. The edge geometry itself lives on `hoveredEdgeRef`; the
  // state mirror just serves as a render trigger.
  useEffect(() => {
    const layer = sceneRef.current.hoverEdgeGraphics;
    if (!layer) return;
    for (const c of layer.removeChildren()) c.destroy({ children: true });
    const e = hoveredEdgeRef.current;
    if (!e) return;
    const color =
      e.kind === "implements"
        ? 0x8b5cf6
        : e.kind === "union"
          ? 0xeab308
          : e.kind === "arg"
            ? 0xf97316
            : 0x3b82f6;
    layer.addChild(buildEdgeBatchMesh([e], { colorHex: color, alphaScale: 1 }, 1, 4));
  }, [hoveredEdgeInfo]);

  // Bold stroke overlay for the currently-focused edge so the
  // selected line stands out from the surrounding (dimmed) edges.
  // Drawn beneath the hover overlay so hovering still wins the visual
  // emphasis when both states apply to the same edge.
  useEffect(() => {
    const layer = sceneRef.current.focusEdgeGraphics;
    if (!layer) return;
    for (const c of layer.removeChildren()) c.destroy({ children: true });
    const e = focusedEdge;
    if (!e) return;
    const color =
      e.kind === "implements"
        ? 0x8b5cf6
        : e.kind === "union"
          ? 0xeab308
          : e.kind === "arg"
            ? 0xf97316
            : 0x3b82f6;
    layer.addChild(buildEdgeBatchMesh([e], { colorHex: color, alphaScale: 1 }, 1, 5));
  }, [focusedEdge]);

  // FPS + timing overlay state
  const fpsOverlayRef = useRef<HTMLDivElement>(null);

  // Background color sync — runs on theme change AND when app first
  // becomes ready (async init may finish after the initial effect).
  useEffect(() => {
    const app = appRef.current;
    if (!app) return;
    const bgColor = getComputedCssVar("--background", "#ffffff");
    const bgHex = cssColorToHex(bgColor);
    app.renderer.background.color = bgHex;
  }, [themeResolved, appReady]);

  return (
    <div
      ref={containerRef}
      className="relative h-full w-full overflow-hidden bg-background"
      onMouseDown={onMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={endDrag}
      onMouseLeave={endDrag}
      onClick={onClick}
      style={{ cursor, touchAction: "none" }}
    >
      <div ref={pixiContainerRef} style={{ width: size.w, height: size.h }} />

      <div
        className={cn(
          "pointer-events-auto absolute left-4 top-4 z-20 flex items-center gap-1.5 rounded-lg border border-border bg-popover/95 px-2 py-1.5 font-mono text-xs text-popover-foreground opacity-40 shadow-lg backdrop-blur transition-[opacity,transform] duration-300 ease-out hover:opacity-100",
          leftControlsInset && "translate-y-11",
        )}
        onMouseMove={(ev) => ev.stopPropagation()}
        onClick={(ev) => ev.stopPropagation()}
      >
        <button
          type="button"
          onClick={() => setHidePrimitiveFields(!hidePrimitiveFields)}
          className={cn(
            "flex cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium transition-colors",
            hidePrimitiveFields
              ? "border-primary bg-primary/10 text-primary"
              : "border-border text-muted-foreground hover:border-border/80 hover:text-foreground",
          )}
        >
          <Filter className="h-2.5 w-2.5" />
          Hide primitives
        </button>
        <button
          type="button"
          onClick={() => setHideRelayBoilerplate(!hideRelayBoilerplate)}
          className={cn(
            "flex cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium transition-colors",
            hideRelayBoilerplate
              ? "border-primary bg-primary/10 text-primary"
              : "border-border text-muted-foreground hover:border-border/80 hover:text-foreground",
          )}
          title="Hide the Relay Node interface, PageInfo, and *Edge / *Connection types"
        >
          <Filter className="h-2.5 w-2.5" />
          Hide Relay
        </button>
        <button
          type="button"
          onClick={() => setShowGraphDescriptions((v) => !v)}
          className={cn(
            "flex cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium transition-colors",
            showGraphDescriptions
              ? "border-primary bg-primary/10 text-primary"
              : "border-border text-muted-foreground hover:border-border/80 hover:text-foreground",
          )}
          title="Render SDL descriptions inline on each node card (taller rows; re-runs layout)"
        >
          <Filter className="h-2.5 w-2.5" />
          Show descriptions
        </button>
      </div>

      <div
        className={cn(
          "pointer-events-auto absolute left-4 top-14 z-20 flex flex-col gap-1.5 rounded-lg border border-border bg-popover/95 px-2 py-1.5 font-mono text-xs text-popover-foreground opacity-40 shadow-lg backdrop-blur transition-[opacity,transform] duration-300 ease-out hover:opacity-100",
          leftControlsInset && "translate-y-11",
        )}
        onMouseMove={(ev) => ev.stopPropagation()}
        onClick={(ev) => ev.stopPropagation()}
      >
        <div className="flex items-center gap-1.5">
          <Microscope className="h-3 w-3 shrink-0 text-muted-foreground" />
          <span className="text-[10px] uppercase tracking-wider text-muted-foreground">
            Investigate
          </span>
        </div>
        <div className="flex items-center gap-1.5">
          <button
            type="button"
            onClick={() =>
              setInvestigateMode((m) => (m === "description" ? "off" : "description"))
            }
            className={cn(
              "flex cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-medium transition-colors",
              investigateMode === "description"
                ? "border-orange-500 bg-orange-500/10 text-orange-500"
                : "border-border text-muted-foreground hover:border-border/80 hover:text-foreground",
            )}
            title="Highlight nodes whose descriptions are missing"
          >
            Missing descriptions
          </button>
          <span
            className={cn(
              "rounded-full px-1.5 py-0.5 text-[10px] tabular-nums",
              descriptionCoverage >= 0.9
                ? "bg-emerald-500/10 text-emerald-500"
                : descriptionCoverage >= 0.5
                  ? "bg-amber-500/10 text-amber-500"
                  : "bg-rose-500/10 text-rose-500",
            )}
            title={`${(descriptionCoverage * 100).toFixed(1)}% of documentable items have a description`}
          >
            {Math.round(descriptionCoverage * 100)}%
          </span>
        </div>
      </div>

      {clickHistory.length > 0 && (
        <div
          className="pointer-events-auto absolute right-4 top-4 z-20 flex w-64 max-w-[40vw] flex-col rounded-lg border border-border bg-popover/95 font-mono text-xs text-popover-foreground opacity-40 shadow-lg backdrop-blur transition-opacity duration-150 hover:opacity-100"
          // Swallow mouse moves so the canvas's hover hit-tests don't
          // fire while the cursor is on the history panel. Click is
          // swallowed too so the canvas's onClick doesn't treat a
          // panel-button click as "click on empty space" and immediately
          // clear the focus state our handler just set.
          onMouseMove={(ev) => ev.stopPropagation()}
          onClick={(ev) => ev.stopPropagation()}
          onMouseEnter={() => {
            hoveredFieldRef.current = null;
            hoveredNodeRef.current = null;
            if (hoveredEdgeRef.current !== null) {
              hoveredEdgeRef.current = null;
              setHoveredEdgeInfo(null);
            }
            if (hoveredNodeForTipRef.current !== null) {
              hoveredNodeForTipRef.current = null;
              setHoveredNodeTip(null);
            }
          }}
        >
          <div className="flex items-center gap-2 border-b border-border px-3 py-2">
            <History className="h-3 w-3 text-muted-foreground" />
            <span className="flex-1 text-[10px] uppercase tracking-wider text-muted-foreground">
              Recent ({clickHistory.length})
            </span>
            <button
              type="button"
              onClick={() => setClickHistory([])}
              className="rounded p-1 text-muted-foreground hover:bg-secondary hover:text-foreground"
              title="Clear history"
            >
              <Trash2 className="h-3 w-3" />
            </button>
            <button
              type="button"
              onClick={() => setHistoryOpen((v) => !v)}
              className="rounded p-1 text-muted-foreground hover:bg-secondary hover:text-foreground"
              title={historyOpen ? "Collapse" : "Expand"}
            >
              {historyOpen ? (
                <ChevronUp className="h-3 w-3" />
              ) : (
                <ChevronDown className="h-3 w-3" />
              )}
            </button>
          </div>
          {historyOpen && (
            <ul className="max-h-[60vh] overflow-auto py-1">
              {clickHistory.map((item) => {
                if (item.kind === "node") {
                  const style = KIND_STYLES[item.nodeKind];
                  return (
                    <li key={`${item.id}:${item.ts}`} className="group flex items-center px-2 py-0.5">
                      <button
                        type="button"
                        onClick={() => {
                          setFocusedEdge(null);
                          onNavigate?.(item.nodeId);
                        }}
                        onMouseEnter={(ev) => {
                          setHoveredHistoryItem(item);
                          moveHistoryTip(ev.clientX, ev.clientY);
                        }}
                        onMouseMove={(ev) => moveHistoryTip(ev.clientX, ev.clientY)}
                        onMouseLeave={() => setHoveredHistoryItem(null)}
                        className={cn(
                          "flex min-w-0 flex-1 cursor-pointer items-center gap-2 rounded px-2 py-1 text-left transition-colors hover:brightness-110",
                          style.header,
                        )}
                      >
                        <span className={cn("rounded px-1 py-0 text-[9px] uppercase tracking-wide", style.badge)}>
                          {style.label}
                        </span>
                        <span className="truncate">{item.name}</span>
                      </button>
                      <button
                        type="button"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          removeFromHistory(item.id);
                        }}
                        className="mr-2 shrink-0 rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-secondary hover:text-foreground group-hover:opacity-100"
                        title="Remove from history"
                      >
                        <X className="h-3 w-3" />
                      </button>
                    </li>
                  );
                }
                if (item.kind === "field") {
                  const style = KIND_STYLES[item.nodeKind];
                  return (
                    <li key={`${item.id}:${item.ts}`} className="group flex items-center px-2 py-0.5">
                      <button
                        type="button"
                        onClick={() => {
                          setFocusedEdge(null);
                          // Re-pin → re-frames the owner node and
                          // re-draws the field-row highlight.
                          setPinnedField({
                            typeId: item.typeId,
                            fieldName: item.fieldName,
                            fieldIndex: item.fieldIndex,
                          });
                        }}
                        onMouseEnter={(ev) => {
                          setHoveredHistoryItem(item);
                          moveHistoryTip(ev.clientX, ev.clientY);
                        }}
                        onMouseMove={(ev) => moveHistoryTip(ev.clientX, ev.clientY)}
                        onMouseLeave={() => setHoveredHistoryItem(null)}
                        className={cn(
                          "flex min-w-0 flex-1 cursor-pointer items-center gap-2 rounded px-2 py-1 text-left transition-colors hover:brightness-110",
                          style.header,
                        )}
                      >
                        <span className={cn("rounded px-1 py-0 text-[9px] uppercase tracking-wide", style.badge)}>
                          {style.label}
                        </span>
                        <span className="min-w-0 flex-1 truncate">
                          {item.typeName}
                          <span className="text-muted-foreground">.</span>
                          <span style={{ color: "#f59e0b" }}>{item.fieldName}</span>
                        </span>
                      </button>
                      <button
                        type="button"
                        onClick={(ev) => {
                          ev.stopPropagation();
                          removeFromHistory(item.id);
                        }}
                        className="mr-2 shrink-0 rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-secondary hover:text-foreground group-hover:opacity-100"
                        title="Remove from history"
                      >
                        <X className="h-3 w-3" />
                      </button>
                    </li>
                  );
                }
                const sourceKind = nodeById.get(item.sourceId)?.data.kind;
                const targetKind = nodeById.get(item.targetId)?.data.kind;
                const sourceStyle = sourceKind ? KIND_STYLES[sourceKind] : null;
                const targetStyle = targetKind ? KIND_STYLES[targetKind] : null;
                return (
                  <li key={`${item.id}:${item.ts}`} className="group flex items-center px-2 py-0.5">
                    <button
                      type="button"
                      onMouseEnter={(ev) => {
                        setHoveredHistoryItem(item);
                        moveHistoryTip(ev.clientX, ev.clientY);
                      }}
                      onMouseMove={(ev) => moveHistoryTip(ev.clientX, ev.clientY)}
                      onMouseLeave={() => setHoveredHistoryItem(null)}
                      onClick={() => {
                        // Re-locate the edge in the current layout
                        // (the original LaidEdge reference may be
                        // stale after a re-layout). Falls back to
                        // navigating to the source type if not found.
                        const live = laidEdges.find(
                          (e) =>
                            e.sourceId === item.sourceId &&
                            e.targetId === item.targetId &&
                            (e.label ?? "") === item.label &&
                            e.kind === item.edgeKind,
                        );
                        if (live) focusOnEdge(live);
                        else onNavigate?.(item.sourceId);
                      }}
                      className="flex min-w-0 flex-1 cursor-pointer items-center gap-1 rounded px-2 py-1 text-left transition-colors hover:bg-secondary/60"
                    >
                      {sourceStyle && (
                        <span className={cn("rounded px-1 py-0 text-[9px] uppercase tracking-wide", sourceStyle.badge)}>
                          {sourceStyle.label}
                        </span>
                      )}
                      <span className="min-w-0 flex-1 truncate">
                        {(item.edgeKind === "field" || item.edgeKind === "arg") && item.label ? (
                          <>
                            {item.sourceId}
                            <span className="text-muted-foreground">.</span>
                            <span style={{ color: "#f59e0b" }}>{item.label}</span>
                          </>
                        ) : item.edgeKind === "implements" ? (
                          <>
                            <span className="text-muted-foreground italic">↳ </span>
                            {item.targetId}
                          </>
                        ) : item.edgeKind === "union" ? (
                          <>
                            {item.sourceId}
                            <span className="text-muted-foreground"> | </span>
                            {item.targetId}
                          </>
                        ) : (
                          item.label || `${item.sourceId} → ${item.targetId}`
                        )}
                      </span>
                      <ArrowRight className="h-3 w-3 shrink-0 text-muted-foreground" />
                      {targetStyle && (
                        <span className={cn("rounded px-1 py-0 text-[9px] uppercase tracking-wide", targetStyle.badge)}>
                          {targetStyle.label}
                        </span>
                      )}
                    </button>
                    <button
                      type="button"
                      onClick={(ev) => {
                        ev.stopPropagation();
                        removeFromHistory(item.id);
                      }}
                      className="mr-2 shrink-0 rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-secondary hover:text-foreground group-hover:opacity-100"
                      title="Remove from history"
                    >
                      <X className="h-3 w-3" />
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      )}

      {hoveredHistoryItem && (() => {
        const item = hoveredHistoryItem;
        if (item.kind === "node") {
          return (
            <div
              ref={historyTipElRef}
              className="pointer-events-none fixed z-50 whitespace-nowrap rounded-lg border border-border bg-popover/95 px-3 py-2 font-mono text-xs text-popover-foreground shadow-lg backdrop-blur"
              style={tooltipStyle(hoveredHistoryPosRef.current.x, hoveredHistoryPosRef.current.y)}
            >
              <div className="flex items-center gap-2">
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[item.nodeKind] }}
                >
                  {KIND_STYLES[item.nodeKind].label}
                </span>
                <span className="font-semibold">{item.name}</span>
              </div>
            </div>
          );
        }
        if (item.kind === "field") {
          return (
            <div
              ref={historyTipElRef}
              className="pointer-events-none fixed z-50 whitespace-nowrap rounded-lg border border-border bg-popover/95 px-3 py-2 font-mono text-xs text-popover-foreground shadow-lg backdrop-blur"
              style={tooltipStyle(hoveredHistoryPosRef.current.x, hoveredHistoryPosRef.current.y)}
            >
              <div className="flex items-center gap-2">
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[item.nodeKind] }}
                >
                  {KIND_STYLES[item.nodeKind].label}
                </span>
                <span>
                  <span className="font-semibold">{item.typeName}</span>
                  <span className="text-muted-foreground">.</span>
                  <span style={{ color: "#f59e0b" }}>{item.fieldName}</span>
                </span>
              </div>
            </div>
          );
        }
        const sourceKind = nodeById.get(item.sourceId)?.data.kind;
        const targetKind = nodeById.get(item.targetId)?.data.kind;
        return (
          <div
            ref={historyTipElRef}
            className="pointer-events-none fixed z-50 whitespace-nowrap rounded-lg border border-border bg-popover/95 px-3 py-2 font-mono text-xs text-popover-foreground shadow-lg backdrop-blur"
            style={tooltipStyle(hoveredHistoryPosRef.current.x, hoveredHistoryPosRef.current.y)}
          >
            <div className="flex items-center gap-2">
              {sourceKind && (
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[sourceKind] }}
                >
                  {KIND_STYLES[sourceKind].label}
                </span>
              )}
              {(item.edgeKind === "field" || item.edgeKind === "arg") && item.label ? (
                <span>
                  <span className="font-semibold">{item.sourceId}</span>
                  <span className="text-muted-foreground">.</span>
                  <span style={{ color: "#f59e0b" }}>{item.label}</span>
                </span>
              ) : (
                <span className="font-semibold">{item.sourceId}</span>
              )}
              {item.edgeKind === "implements" && (
                <span className="text-muted-foreground italic">implements</span>
              )}
              {item.edgeKind === "union" && (
                <span className="text-muted-foreground">|</span>
              )}
              <ArrowRight className="h-3 w-3 text-muted-foreground" />
              {targetKind && (
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[targetKind] }}
                >
                  {KIND_STYLES[targetKind].label}
                </span>
              )}
              <span className="font-semibold">{item.targetId}</span>
            </div>
          </div>
        );
      })()}

      {hoveredEdgeInfo && (() => {
        const sourceKind = nodeById.get(hoveredEdgeInfo.sourceId)?.data.kind;
        const targetKind = nodeById.get(hoveredEdgeInfo.targetId)?.data.kind;
        return (
          <div
            ref={edgeTipElRef}
            className="pointer-events-none fixed z-50 whitespace-nowrap rounded-lg border border-border bg-popover/95 px-3 py-2 font-mono text-xs text-popover-foreground shadow-lg backdrop-blur"
            style={tooltipStyle(hoveredEdgeScreenRef.current.x, hoveredEdgeScreenRef.current.y)}
          >
            <div className="flex items-center gap-2">
              {sourceKind && (
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[sourceKind] }}
                >
                  {KIND_STYLES[sourceKind].label}
                </span>
              )}
              {(hoveredEdgeInfo.kind === "field" || hoveredEdgeInfo.kind === "arg") && hoveredEdgeInfo.label ? (
                <span>
                  <span className="font-semibold">{hoveredEdgeInfo.sourceId}</span>
                  <span className="text-muted-foreground">.</span>
                  <span style={{ color: "#f59e0b" }}>{hoveredEdgeInfo.label}</span>
                </span>
              ) : (
                <span className="font-semibold">{hoveredEdgeInfo.sourceId}</span>
              )}
              {hoveredEdgeInfo.kind === "implements" && (
                <span className="text-muted-foreground italic">implements</span>
              )}
              {hoveredEdgeInfo.kind === "union" && (
                <span className="text-muted-foreground">|</span>
              )}
              <ArrowRight className="h-3 w-3 text-muted-foreground" />
              {targetKind && (
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[targetKind] }}
                >
                  {KIND_STYLES[targetKind].label}
                </span>
              )}
              <span className="font-semibold">{hoveredEdgeInfo.targetId}</span>
            </div>
          </div>
        );
      })()}

      {focusedEdge && (() => {
        const e = focusedEdge;
        const label = e.label ?? "";
        const sourceKind = nodeById.get(e.sourceId)?.data.kind;
        const targetKind = nodeById.get(e.targetId)?.data.kind;
        return (
          <div
            className="pointer-events-auto absolute bottom-4 left-1/2 z-20 max-w-[92vw] -translate-x-1/2 rounded-lg border border-border bg-popover/95 px-3 py-2 font-mono text-xs text-popover-foreground shadow-lg backdrop-blur"
            // Swallow mouse moves so the canvas's hover hit-tests don't
            // fire while the cursor is on the focused-edge tooltip.
            // Click is swallowed too so the canvas's onClick doesn't
            // treat the X button click as "click on empty space" and
            // double-clear the focus state.
            onMouseMove={(ev) => ev.stopPropagation()}
            onClick={(ev) => ev.stopPropagation()}
            onMouseEnter={() => {
              hoveredFieldRef.current = null;
              hoveredNodeRef.current = null;
              if (hoveredEdgeRef.current !== null) {
                hoveredEdgeRef.current = null;
                setHoveredEdgeInfo(null);
              }
              if (hoveredNodeForTipRef.current !== null) {
                hoveredNodeForTipRef.current = null;
                setHoveredNodeTip(null);
              }
            }}
          >
            <div className="flex flex-wrap items-center gap-2">
              {sourceKind && (
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[sourceKind] }}
                >
                  {KIND_STYLES[sourceKind].label}
                </span>
              )}
              {(e.kind === "field" || e.kind === "arg") && label ? (
                <span>
                  <span className="font-semibold">{e.sourceId}</span>
                  <span className="text-muted-foreground">.</span>
                  <span style={{ color: "#f59e0b" }}>{label}</span>
                </span>
              ) : (
                <span className="font-semibold">{e.sourceId}</span>
              )}
              {e.kind === "implements" && (
                <span className="text-muted-foreground italic">implements</span>
              )}
              {e.kind === "union" && (
                <span className="text-muted-foreground">|</span>
              )}
              <ArrowRight className="h-3 w-3 text-muted-foreground" />
              {targetKind && (
                <span
                  className="rounded px-1 py-0 text-[9px] uppercase tracking-wide text-white"
                  style={{ backgroundColor: KIND_COLORS[targetKind] }}
                >
                  {KIND_STYLES[targetKind].label}
                </span>
              )}
              <span className="font-semibold">{e.targetId}</span>
              <button
                type="button"
                onClick={(ev) => { ev.stopPropagation(); setFocusedEdge(null); }}
                className="ml-2 rounded p-1 text-muted-foreground hover:bg-secondary hover:text-foreground"
                title="Clear edge focus"
              >
                <X className="h-3 w-3" />
              </button>
            </div>
          </div>
        );
      })()}

      {hoveredNodeTip &&
        (currentLodRef.current !== "full" ||
          viewRef.current.k < FIELD_CLICK_MIN_ZOOM) && (
        <div
          ref={nodeTipElRef}
          className="pointer-events-none fixed z-50 flex items-center gap-1.5 whitespace-nowrap rounded-md border border-border bg-popover px-2 py-1 font-mono text-[11px] text-popover-foreground shadow-md"
          style={tooltipStyle(hoveredNodeScreenRef.current.x, hoveredNodeScreenRef.current.y)}
        >
          <span
            className="rounded px-1 py-0 text-[9px] uppercase tracking-wide"
            style={{
              backgroundColor: KIND_COLORS[hoveredNodeTip.kind],
              color: "white",
            }}
          >
            {KIND_STYLES[hoveredNodeTip.kind].label}
          </span>
          <span>{hoveredNodeTip.name}</span>
        </div>
      )}

      {isPending && (
        <div
          className="pointer-events-auto absolute inset-0 z-30 flex cursor-wait items-center justify-center bg-background/60 backdrop-blur-sm"
          role="status"
          aria-live="polite"
          // Block all canvas interactions while a layout is in-flight
          // so hover hit-tests / clicks don't fire against stale
          // (mid-rebuild) positions.
          onMouseMove={(ev) => ev.stopPropagation()}
          onMouseDown={(ev) => ev.stopPropagation()}
          onClick={(ev) => ev.stopPropagation()}
          onWheel={(ev) => ev.stopPropagation()}
        >
          <div className="flex w-72 flex-col items-center gap-3 rounded-xl border border-border bg-card/90 px-6 py-5 shadow-lg">
            <Loader2 className="h-7 w-7 animate-spin text-primary" />
            <div className="text-sm font-medium">
              Laying out {nodes.length.toLocaleString()} types…
            </div>
            {layoutProgress && layoutProgress.total > 0 ? (
              <>
                <div className="h-1.5 w-full overflow-hidden rounded-full bg-secondary">
                  <div
                    className="h-full rounded-full bg-primary transition-[width] duration-150"
                    style={{
                      width: `${Math.round(
                        (layoutProgress.done / layoutProgress.total) * 100,
                      )}%`,
                    }}
                  />
                </div>
                <div className="font-mono text-[10px] text-muted-foreground">
                  {layoutProgress.done} / {layoutProgress.total} chunks ·{" "}
                  {Math.round(
                    (layoutProgress.done / layoutProgress.total) * 100,
                  )}
                  %
                </div>
              </>
            ) : (
              <div className="text-xs text-muted-foreground">
                Large schemas may take a few seconds.
              </div>
            )}
          </div>
        </div>
      )}

      {/* FPS overlay with real-time chart */}
      <div
        ref={fpsOverlayRef}
        className="pointer-events-none absolute bottom-4 right-4 rounded-lg border border-border/20 bg-background/10 px-3 py-2 font-mono text-xs text-muted-foreground/60 backdrop-blur-sm"
        style={{ minWidth: 280 }}
      >
        <canvas
          ref={chartCanvasRef}
          width={260}
          height={48}
          className="mb-1.5 rounded"
          style={{ width: 260, height: 48, display: "block" }}
        />
        <div className="flex items-baseline justify-between gap-4">
          <span ref={fpsTextRef}>0 fps</span>
          <span>{laidNodes.length} nodes · {laidEdges.length} edges</span>
        </div>
        {lastTiming && (
          <div className="mt-1 space-y-0.5 opacity-70">
            {lastTiming.fromCache ? (
              <div>cached · total {lastTiming.totalMs.toFixed(0)}ms</div>
            ) : (
              <>
                <div>similarity {lastTiming.similarityMs.toFixed(0)}ms max</div>
                <div>layout {lastTiming.layoutMs.toFixed(0)}ms · total {lastTiming.totalMs.toFixed(0)}ms</div>
                <div>
                  {lastTiming.componentCount} comp · {lastTiming.singletonCount} singletons · {lastTiming.parallelWorkers}w
                </div>
                {lastTiming.fallbackNodeCount > 0 && (
                  <div>fallback grid: {lastTiming.fallbackNodeCount} nodes</div>
                )}
              </>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
