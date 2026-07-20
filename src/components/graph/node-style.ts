import type { NodeKind } from "@/lib/sdl-to-graph";

interface KindStyle {
  label: string;
  ring: string;
  header: string;
  badge: string;
}

/**
 * Tailwind-class style map used by the tree panel / badge UIs.
 * SVG rendering uses KIND_COLORS below (raw color strings).
 */
export const KIND_STYLES: Record<NodeKind, KindStyle> = {
  Object: {
    label: "type",
    ring: "border-sky-500/60",
    header: "bg-sky-500/10 text-sky-600 dark:text-sky-300",
    badge: "bg-sky-500 text-white",
  },
  Interface: {
    label: "interface",
    ring: "border-violet-500/60",
    header: "bg-violet-500/10 text-violet-600 dark:text-violet-300",
    badge: "bg-violet-500 text-white",
  },
  Union: {
    label: "union",
    ring: "border-amber-500/60",
    header: "bg-amber-500/10 text-amber-600 dark:text-amber-300",
    badge: "bg-amber-500 text-white",
  },
  Enum: {
    label: "enum",
    ring: "border-emerald-500/60",
    header: "bg-emerald-500/10 text-emerald-600 dark:text-emerald-300",
    badge: "bg-emerald-500 text-white",
  },
  Scalar: {
    label: "scalar",
    ring: "border-rose-500/60",
    header: "bg-rose-500/10 text-rose-600 dark:text-rose-300",
    badge: "bg-rose-500 text-white",
  },
  Input: {
    label: "input",
    ring: "border-fuchsia-500/60",
    header: "bg-fuchsia-500/10 text-fuchsia-600 dark:text-fuchsia-300",
    badge: "bg-fuchsia-500 text-white",
  },
};

/**
 * Raw color values for SVG rendering. Derived from Tailwind's *-500 palette.
 * Per spec (rule 6) only type/input/union/interface are colored distinctly;
 * enum and scalar reuse type/interface tones.
 */
export const KIND_COLORS: Record<NodeKind, string> = {
  Object: "#0ea5e9", // sky-500
  Interface: "#8b5cf6", // violet-500
  Union: "#f59e0b", // amber-500
  Enum: "#10b981", // emerald-500
  Scalar: "#f43f5e", // rose-500
  Input: "#d946ef", // fuchsia-500
};

export const KIND_COLORS_DARK: Record<NodeKind, string> = {
  Object: "#0369a1", // sky-700
  Interface: "#5b21b6", // violet-800
  Union: "#b45309", // amber-700
  Enum: "#047857", // emerald-700
  Scalar: "#be123c", // rose-700
  Input: "#a21caf", // fuchsia-700
};

export const NODE_WIDTH = 220;
export const HEADER_H = 42;
export const ROW_H = 14;
export const TOP_BODY_PAD = 8;
export const BOTTOM_PAD = 10;
/** Vertical gap inserted between the last field row and the
 *  violet `implements` section. Without it the section pressed up
 *  against the previous field's description text — adding a small
 *  breathing strip makes the two zones read as separate. */
export const IMPL_SECTION_GAP = 8;

// Description-mode dimensions. When the "Show descriptions" toggle is
// on, every field row gets an extra italic line below the name and the
// header carries a wrapped type-description block beneath the type
// name. These reserve the extra vertical space deterministically (one
// description line per row, regardless of whether the row's
// description is empty) so the layout stays predictable.
export const ROW_H_WITH_DESC = 26;
export const HEADER_H_WITH_DESC = HEADER_H + 14;

export function rowHFor(showDescriptions: boolean): number {
  return showDescriptions ? ROW_H_WITH_DESC : ROW_H;
}
export function headerHFor(showDescriptions: boolean): number {
  return showDescriptions ? HEADER_H_WITH_DESC : HEADER_H;
}

export function estimateNodeHeight(
  kind: NodeKind,
  fieldCount = 0,
  valueCount = 0,
  memberCount = 0,
  interfaceCount = 0,
  showDescriptions = false,
  /** For Object types: number of Union types this type is a member
   *  of — rendered as an extra amber section below `implements`. */
  unionCount = 0,
): number {
  const rowH = rowHFor(showDescriptions);
  const headerH = headerHFor(showDescriptions);
  const rows = kind === "Enum" ? valueCount : kind === "Scalar" ? 0 : kind === "Union" ? 0 : fieldCount;
  // Rows that never carry a description line — implements /
  // union-membership sections and Union member rows — always use the
  // tight base ROW_H even in descriptions mode.
  const tightRows =
    kind === "Union"
      ? memberCount
      : kind === "Object" || kind === "Interface"
        ? interfaceCount + unionCount
        : 0;
  const body =
    rows === 0 && tightRows === 0 ? rowH : rows * rowH + tightRows * ROW_H;
  // Add a small gap between the last field row and the implements
  // section so they don't collide visually (only applies when the
  // node actually has both fields AND interfaces).
  const implGap =
    (kind === "Object" || kind === "Interface") &&
    interfaceCount > 0 &&
    fieldCount > 0
      ? IMPL_SECTION_GAP
      : 0;
  // Same breathing strip above the union-membership section — but only
  // against plain field rows. When an implements section sits directly
  // above, the two colored bands touch instead.
  const unionGap =
    kind === "Object" && unionCount > 0 && interfaceCount === 0 && fieldCount > 0
      ? IMPL_SECTION_GAP
      : 0;
  return headerH + TOP_BODY_PAD + body + implGap + unionGap + BOTTOM_PAD;
}

/** Font used for the name row in a node header — referenced from both
 *  the width estimator and the canvas renderer so they agree. */
export const NODE_NAME_FONT = "600 13px ui-monospace, SFMono-Regular, Menlo, monospace";

// Header side-padding (left + right) factored into the width budget.
const NAME_H_PAD = 16;
const NODE_MIN_WIDTH = 220;
const NODE_MAX_WIDTH = 900;

// Global width multiplier — nodes render this much wider than the
// name+pad requirement, so field-name / field-type rows have plenty
// of breathing room and their truncation limits can grow

const NODE_WIDTH_SCALE = 1.0;

let measureCtx: CanvasRenderingContext2D | null = null;
function getMeasureCtx(): CanvasRenderingContext2D | null {
  if (measureCtx) return measureCtx;
  if (typeof document === "undefined") return null;
  const c = document.createElement("canvas");
  const ctx = c.getContext("2d");
  if (ctx) measureCtx = ctx;
  return measureCtx;
}

/**
 * Width needed so the given type name renders in full in the node
 * header. We measure the actual pixel width in the header font rather
 * than guessing a per-char average — GraphViz monospace glyphs vary
 * wider than expected once bold is applied, so a character-count
 * estimate was letting real schemas still ellipse.
 *
 * Clamped between a sane minimum (so short names don't produce tiny
 * nodes) and a maximum (so one absurdly long name doesn't explode
 * layout). The renderer should secondary-truncate if an individual
 * name still overflows the clamped width.
 */

export function estimateNodeWidth(name: string, fields: [string, string][]): number {
  const ctx = getMeasureCtx();
  let textW: number;
  let fieldMax: number = 0;
  if (ctx) {
    ctx.font = NODE_NAME_FONT;
    textW = ctx.measureText(name).width;
    for (const field of fields) {
      fieldMax = Math.max(
        fieldMax,
        ctx.measureText(field[0]).width + NAME_H_PAD + ctx.measureText(field[1]).width,
      );
    }
  } else {
    // SSR / no DOM fallback: conservative per-char estimate.
    textW = name.length * 9;
  }
  const required = NAME_H_PAD + Math.ceil(textW);
  const base = Math.max(NODE_MIN_WIDTH, Math.min(NODE_MAX_WIDTH, required), fieldMax);
  return Math.round(base * NODE_WIDTH_SCALE);
}

export const NODE_DIMENSIONS = { width: NODE_WIDTH };
