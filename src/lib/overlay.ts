/**
 * The overlay lets a user drop extra SDL on top of the schema they
 * already loaded — a proposed field, a type they're sketching — and see
 * it wired into the graph without touching the loaded schema.
 *
 * The document that gets parsed is `${base}\n\n${overlay}`, so
 * `extend type Query { … }` grows the real Query node and new types link
 * up to existing ones for free. Two things concatenation can't express
 * are handled by rewriting the overlay first (see {@link prepareOverlay}):
 *
 *   - **Augmenting.** A bare `type User { … }` naming a type the schema
 *     already declares is turned into `extend type User { … }`, so you
 *     can paste a type straight out of the schema, add a field, and have
 *     it merge instead of collide.
 *   - **Removing.** SDL has no syntax for taking something away, so a
 *     line of the form `-name` is lifted out of the text and passed to
 *     the parser as a {@link SchemaRemoval} instead.
 *
 * Both read top to bottom: for any one name, the last line that mentions
 * it wins, so `-meh` above a `meh: Meh!` is just a rewrite while `-meh`
 * below it is a deletion.
 *
 * Restating a field the type already has is an override, not a clash —
 * that's `overrideDuplicates` on the parse, so changing a field's type is
 * a matter of writing the field again with the type you want.
 *
 * The last thing concatenation loses is provenance — which nodes and
 * fields came from the overlay — which {@link markOverlay} recovers by
 * diffing the merged graph against the base one.
 */

import type { GraphNodeData, ParsedGraph, SchemaRemoval } from "./sdl-to-graph";
import { Kind, parse } from "graphql";

/** Joins base and overlay SDL into the single document that gets parsed. */
export function mergeSdl(base: string, overlay: string): string {
  if (!overlay.trim()) return base;
  return `${base}\n\n${overlay}`;
}

export interface OverlayDiff {
  /** Types present only in the merged graph. */
  addedTypes: string[];
  /** `TypeName.fieldName` for rows added to a pre-existing type. */
  addedFields: string[];
  /** `TypeName.fieldName` for rows the overlay restated with a different
   *  shape — a new type, different args, a changed deprecation. */
  changedFields: string[];
  /** Types the overlay took away with a top-level `-TypeName`. */
  removedTypes: string[];
  /** `TypeName.fieldName` for rows the overlay took away with `-name`. */
  removedFields: string[];
}

/**
 * What an SDL document declares: every type name mapped to its member
 * names (fields, enum values, union members). The keys are the set an
 * overlay augments rather than redefines; the values are what a
 * `-Type` followed by a fresh declaration has to clear out.
 *
 * Extensions don't count — they name a type someone else declared.
 * Unparseable input yields an empty map, which simply means nothing gets
 * rewritten.
 */
export function declaredMembers(sdl: string): Map<string, Set<string>> {
  const members = new Map<string, Set<string>>();
  if (!sdl.trim()) return members;
  let doc;
  try {
    doc = parse(sdl);
  } catch {
    return members;
  }
  for (const def of doc.definitions) {
    switch (def.kind) {
      case Kind.OBJECT_TYPE_DEFINITION:
      case Kind.INTERFACE_TYPE_DEFINITION:
      case Kind.INPUT_OBJECT_TYPE_DEFINITION:
        members.set(def.name.value, new Set((def.fields ?? []).map((f) => f.name.value)));
        break;
      case Kind.ENUM_TYPE_DEFINITION:
        members.set(def.name.value, new Set((def.values ?? []).map((v) => v.name.value)));
        break;
      case Kind.UNION_TYPE_DEFINITION:
        members.set(def.name.value, new Set((def.types ?? []).map((t) => t.name.value)));
        break;
      case Kind.SCALAR_TYPE_DEFINITION:
        members.set(def.name.value, new Set());
        break;
      default:
        break;
    }
  }
  return members;
}

/** Type names an SDL document declares — see {@link declaredMembers}. */
export function declaredTypeNames(sdl: string): Set<string> {
  return new Set(declaredMembers(sdl).keys());
}

export interface PreparedOverlay {
  /** Base plus rewritten overlay, ready to hand to `sdlToGraph`. */
  sdl: string;
  /** What the overlay's `-name` lines asked to delete, in source order.
   *  Pass as `sdlToGraph`'s `remove` option. */
  removals: SchemaRemoval[];
  /** Problems in the overlay's own shorthand — a `-name` that couldn't be
   *  attached to a type. Whether a removal *resolves* is decided later,
   *  by the graph build. */
  warnings: string[];
}

/** Matches a removal line: `-email`, `- email`, or `-User.email`. */
const REMOVE_LINE =
  /^-\s*([_A-Za-z][_A-Za-z0-9]*)(?:\.([_A-Za-z][_A-Za-z0-9]*))?$/;

/** Matches the head of a type-system definition or extension. */
const DEF_LINE =
  /^(extend\s+)?(type|interface|input|enum|union|scalar)\s+([_A-Za-z][_A-Za-z0-9]*)/;

/** Matches the start of a member declaration inside a block: a field
 *  (`name:` / `name(`), or an enum value (bare, or with a directive). */
const MEMBER_LINE = /^([_A-Za-z][_A-Za-z0-9]*)\s*(?::|\(|@|$)/;

/**
 * Member names declared on a definition line that carries its body
 * inline — `extend type User { name: String! }`, `enum S { A, B }`. The
 * split respects nesting so a comma inside an argument list or a default
 * value doesn't look like the start of the next member.
 */
function inlineMemberNames(code: string): string[] {
  const open = code.indexOf("{");
  if (open < 0) return [];
  const close = code.lastIndexOf("}");
  const body = code.slice(open + 1, close > open ? close : undefined);

  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < body.length; i++) {
    const c = body[i]!;
    if (c === "(" || c === "[" || c === "{") depth++;
    else if (c === ")" || c === "]" || c === "}") depth = Math.max(0, depth - 1);
    else if (c === "," && depth === 0) {
      parts.push(body.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(body.slice(start));

  const names: string[] = [];
  for (const part of parts) {
    const m = MEMBER_LINE.exec(part.trim());
    if (m) names.push(m[1]!);
  }
  return names;
}

/**
 * Strips comments and string bodies from one line so brace counting and
 * keyword matching only ever see code. `state.inBlock` carries an open
 * `"""` across lines.
 */
function stripLiterals(line: string, state: { inBlock: boolean }): string {
  let out = "";
  let i = 0;
  while (i < line.length) {
    if (state.inBlock) {
      const end = line.indexOf('"""', i);
      if (end < 0) return out;
      state.inBlock = false;
      i = end + 3;
      continue;
    }
    const ch = line[i]!;
    if (ch === "#") break;
    if (line.startsWith('"""', i)) {
      state.inBlock = true;
      i += 3;
      continue;
    }
    if (ch === '"') {
      i++;
      while (i < line.length) {
        if (line[i] === "\\") {
          i += 2;
          continue;
        }
        if (line[i] === '"') {
          i++;
          break;
        }
        i++;
      }
      continue;
    }
    out += ch;
    i++;
  }
  return out;
}

/**
 * Blanks the description immediately above line `i`, if there is one.
 * `extend type Foo` may not carry a description, so a pasted type that
 * brought its doc comment along would otherwise become a syntax error
 * the moment we rewrite it into an extension.
 */
function blankPrecedingDescription(lines: string[], i: number): void {
  let j = i - 1;
  while (j >= 0 && lines[j]!.trim() === "") j--;
  if (j < 0) return;
  const t = lines[j]!.trim();

  if (t.endsWith('"""')) {
    // A one-line `"""doc"""` opens and closes on the same line; anything
    // else means walking back to the opening delimiter.
    let k = j;
    if (!(t.startsWith('"""') && t.length >= 6)) {
      k = j - 1;
      while (k >= 0 && !lines[k]!.trim().startsWith('"""')) k--;
      if (k < 0) return;
    }
    for (let m = k; m <= j; m++) lines[m] = "";
    return;
  }
  if (/^"([^"\\]|\\.)*"$/.test(t)) lines[j] = "";
}

/**
 * True when anything is left between a block's braces. Used to spot a
 * block that removals emptied out: `type User { -name }` leaves `{ }`,
 * which SDL rejects, so the block has to go rather than be handed to the
 * parser.
 */
function blockHasMembers(lines: readonly string[], start: number, end: number): boolean {
  const state = { inBlock: false };
  const code = lines
    .slice(start, end + 1)
    .map((l) => stripLiterals(l, state))
    .join("\n");
  const open = code.indexOf("{");
  const close = code.lastIndexOf("}");
  if (open < 0 || close < open) return true;
  return code.slice(open + 1, close).trim().length > 0;
}

/**
 * Rewrites overlay SDL into something parseable next to `base`: bare
 * redefinitions of known types become extensions, and `-name` lines are
 * lifted out into removals.
 *
 * Lines are blanked rather than deleted so line numbers keep matching the
 * editor — a parse error still points at the line the user is looking at.
 *
 * The overlay reads top to bottom, so for any one name the last line that
 * mentions it decides its fate:
 *
 *     type Foo {
 *       meh: Meh!    # define
 *       -meh         # …then drop it
 *       meh: Meh!    # …then define it again — this is what stands
 *     }
 *
 * Removals are applied after every extension is folded in (SDL has no way
 * to express them inline), so ordering can't come from position alone —
 * it comes from dropping the removals that a later line overrode.
 */
export function rewriteOverlay(
  overlay: string,
  baseMembers: ReadonlyMap<string, ReadonlySet<string>>,
): { sdl: string; removals: SchemaRemoval[]; warnings: string[] } {
  const lines = overlay.split("\n");
  /** Removals in source order, each tagged with the name it targets. */
  const asked: { removal: SchemaRemoval; key: string }[] = [];
  const warnings: string[] = [];
  const state = { inBlock: false };
  /** Brace nesting, counted over code only. */
  let depth = 0;
  /** Paren nesting — an argument list spanning lines must not have its
   *  arguments mistaken for the enclosing type's fields. */
  let parens = 0;
  /** Type whose block we're inside, for bare `-name` lines. */
  let currentType: string | null = null;
  /** Line the innermost-enclosing top-level definition starts on. */
  let defLine = -1;
  /** First line of the top-level block currently open, if any. */
  let openLine = -1;
  /** Top-level `{ … }` spans, checked for emptiness once closed. */
  const blocks: { start: number; end: number }[] = [];
  /** The overlay's last word on a type (`Foo`) or member (`Foo.bar`). */
  const lastWord = new Map<string, "define" | "remove">();

  for (let i = 0; i < lines.length; i++) {
    const code = stripLiterals(lines[i]!, state);
    const trimmed = code.trim();

    const rm = REMOVE_LINE.exec(trimmed);
    if (rm) {
      const [, first, second] = rm;
      const removal: SchemaRemoval | null = second
        ? { type: first!, member: second }
        : depth === 0
          ? { type: first! }
          : currentType
            ? { type: currentType, member: first! }
            : null;
      if (removal) {
        const key = removal.member ? `${removal.type}.${removal.member}` : removal.type;
        lastWord.set(key, "remove");
        asked.push({ removal, key });
      } else {
        warnings.push(
          `Ignoring "-${first}" on line ${i + 1} — not inside a type block; write "-Type.${first}" to remove a field`,
        );
      }
      lines[i] = "";
      continue;
    }

    if (depth === 0) {
      const def = DEF_LINE.exec(trimmed);
      if (def) {
        const [, alreadyExtend, keyword, name] = def;
        currentType = name!;
        defLine = i;
        lastWord.set(name!, "define");
        // A union declares its members inline, with no block to scan.
        if (keyword === "union") {
          const eq = code.indexOf("=");
          if (eq >= 0) {
            for (const m of code.slice(eq + 1).split("|")) {
              const t = m.trim();
              if (t) lastWord.set(`${name}.${t}`, "define");
            }
          }
        }
        // Everything else may still open and close its body on this one
        // line, which the per-line member scan below would never see.
        for (const m of inlineMemberNames(code)) lastWord.set(`${name}.${m}`, "define");

        if (!alreadyExtend && baseMembers.has(name!)) {
          // `extend scalar X` is only legal with directives attached, and
          // a scalar redeclaration carries nothing to merge anyway — drop
          // the line instead of extending it.
          if (keyword === "scalar") {
            lines[i] = "";
            continue;
          }
          // Unions have no braces; every other kind needs its body on
          // this line for the extension to be well-formed. Anything else
          // is left alone, so the duplicate is reported rather than
          // silently mangled.
          const extendable = keyword === "union" ? code.includes("=") : code.includes("{");
          if (extendable) {
            blankPrecedingDescription(lines, i);
            lines[i] = lines[i]!.replace(/^(\s*)/, "$1extend ");
          }
        }
      }
    }

    // A member declaration sits at brace depth 1 (deeper braces are
    // default values) and outside any argument list.
    else if (depth === 1 && parens === 0 && currentType) {
      const member = MEMBER_LINE.exec(trimmed);
      if (member) lastWord.set(`${currentType}.${member[1]}`, "define");
    }

    for (const ch of code) {
      if (ch === "{") {
        if (depth === 0) openLine = defLine >= 0 ? defLine : i;
        depth++;
      } else if (ch === "}") {
        depth = Math.max(0, depth - 1);
        if (depth === 0) {
          if (openLine >= 0) blocks.push({ start: openLine, end: i });
          openLine = -1;
          currentType = null;
          defLine = -1;
        }
      } else if (ch === "(") parens++;
      else if (ch === ")") parens = Math.max(0, parens - 1);
    }
  }

  // A block whose every member was removed can't be parsed, so it goes
  // away entirely — along with any description that would otherwise
  // dangle in front of the next definition.
  for (const b of blocks) {
    if (blockHasMembers(lines, b.start, b.end)) continue;
    blankPrecedingDescription(lines, b.start);
    for (let m = b.start; m <= b.end; m++) lines[m] = "";
  }

  // Ordering, resolved: a removal only survives if nothing below it put
  // the name back. Declaring the member again is the rewrite — the
  // extension's field then overrides whatever was there.
  const removals = asked
    .filter(({ key }) => lastWord.get(key) === "remove")
    .map(({ removal }) => removal);

  // A type dropped and then declared again is a replacement rather than a
  // deletion: clear out the members the schema had, and let the block
  // below supply the type's new contents.
  for (const { removal, key } of asked) {
    if (removal.member || lastWord.get(key) === "remove") continue;
    for (const m of baseMembers.get(removal.type) ?? []) {
      if (lastWord.get(`${removal.type}.${m}`) === "define") continue;
      removals.push({ type: removal.type, member: m });
    }
  }

  return { sdl: lines.join("\n"), removals, warnings };
}

/** Builds the document to parse from a schema and the SDL laid over it. */
export function prepareOverlay(base: string, overlay: string): PreparedOverlay {
  if (!overlay.trim()) return { sdl: base, removals: [], warnings: [] };
  const rewritten = rewriteOverlay(overlay, declaredMembers(base));
  return {
    sdl: mergeSdl(base, rewritten.sdl),
    removals: rewritten.removals,
    warnings: rewritten.warnings,
  };
}

/** Row names (fields for most kinds, values for enums, members for unions). */
function rowNames(n: GraphNodeData): string[] {
  if (n.kind === "Enum") return (n.values ?? []).map((v) => v.name);
  if (n.kind === "Union") return n.members ?? [];
  return (n.fields ?? []).map((f) => f.name);
}

/**
 * Everything about a row that the card renders, minus its name. Comparing
 * these is how an override is spotted: the row is neither added nor
 * removed, but it isn't what the schema had either. Union members carry
 * nothing beyond their name, so they never register as changed.
 */
function rowShapes(n: GraphNodeData): Map<string, string> {
  const shapes = new Map<string, string>();
  if (n.kind === "Union") return shapes;
  if (n.kind === "Enum") {
    for (const v of n.values ?? []) {
      shapes.set(v.name, `${v.isDeprecated ? "!" : ""}${v.deprecationReason ?? ""}|${v.description ?? ""}`);
    }
    return shapes;
  }
  for (const f of n.fields ?? []) {
    const args = (f.args ?? []).map((a) => `${a.name}:${a.type}`).join(",");
    shapes.set(
      f.name,
      `${f.type}(${args})|${f.isDeprecated ? "!" : ""}${f.deprecationReason ?? ""}|${f.description ?? ""}`,
    );
  }
  return shapes;
}

/**
 * Marks everything the overlay contributed. Returns a copy of the merged
 * graph whose new nodes carry `isOverlay`, and whose pre-existing nodes
 * carry `isOverlay` on just the rows the overlay added — plus a plain
 * summary for the panel to display.
 *
 * Removals show up in the summary only: a row that's gone has nothing
 * left to paint. They're recovered by diffing in the other direction —
 * anything in the base graph the merged graph no longer has.
 *
 * Only nodes that actually changed are cloned, so an overlay touching
 * one type doesn't churn every node identity (which would reset the
 * canvas texture cache for the whole graph).
 */
export function markOverlay(
  base: ParsedGraph,
  merged: ParsedGraph,
): { graph: ParsedGraph; diff: OverlayDiff } {
  const baseRows = new Map<string, Set<string>>();
  const baseShapes = new Map<string, Map<string, string>>();
  for (const n of base.nodes) {
    baseRows.set(n.id, new Set(rowNames(n)));
    baseShapes.set(n.id, rowShapes(n));
  }

  const addedTypes: string[] = [];
  const addedFields: string[] = [];
  const changedFields: string[] = [];
  const removedTypes: string[] = [];
  const removedFields: string[] = [];

  const nodes = merged.nodes.map((n) => {
    const prior = baseRows.get(n.id);
    if (!prior) {
      addedTypes.push(n.name);
      return { ...n, isOverlay: true };
    }

    // Added and overridden rows are both "the overlay's work" as far as
    // the canvas is concerned — one emerald marker covers both — but the
    // diff keeps them apart so the panel can say which is which.
    const priorShapes = baseShapes.get(n.id);
    const shapes = rowShapes(n);
    const isNew = (name: string) => !prior.has(name);
    const isChanged = (name: string) =>
      !isNew(name) && shapes.get(name) !== priorShapes?.get(name);
    const touched = (name: string) => isNew(name) || isChanged(name);
    if (!rowNames(n).some(touched)) return n;

    if (n.kind === "Enum") {
      const values = (n.values ?? []).map((v) =>
        touched(v.name) ? { ...v, isOverlay: true } : v,
      );
      for (const v of values) {
        if (!v.isOverlay) continue;
        (isNew(v.name) ? addedFields : changedFields).push(`${n.name}.${v.name}`);
      }
      return { ...n, values };
    }
    if (n.kind === "Union") {
      // Union members render as plain rows with no per-row metadata, so
      // there is nothing to flag on the node itself — the new member
      // type gets flagged in its own right when it is also new.
      for (const m of n.members ?? []) if (isNew(m)) addedFields.push(`${n.name}.${m}`);
      return n;
    }
    const fields = (n.fields ?? []).map((f) =>
      touched(f.name) ? { ...f, isOverlay: true } : f,
    );
    for (const f of fields) {
      if (!f.isOverlay) continue;
      (isNew(f.name) ? addedFields : changedFields).push(`${n.name}.${f.name}`);
    }
    return { ...n, fields };
  });

  const mergedById = new Map(merged.nodes.map((n) => [n.id, n] as const));
  for (const b of base.nodes) {
    const m = mergedById.get(b.id);
    if (!m) {
      removedTypes.push(b.name);
      continue;
    }
    const kept = new Set(rowNames(m));
    for (const r of rowNames(b)) {
      if (!kept.has(r)) removedFields.push(`${b.name}.${r}`);
    }
  }

  addedTypes.sort();
  addedFields.sort();
  changedFields.sort();
  removedTypes.sort();
  removedFields.sort();
  return {
    graph: { ...merged, nodes },
    diff: { addedTypes, addedFields, changedFields, removedTypes, removedFields },
  };
}
