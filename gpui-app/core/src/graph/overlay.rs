//! SDL overlay. A faithful port of `src/lib/overlay.ts`.
//!
//! The overlay lets a user drop extra SDL on top of the schema they already
//! loaded — a proposed field, a type they're sketching — and see it wired into
//! the graph without touching the loaded schema.
//!
//! The document that gets parsed is `{base}\n\n{overlay}`, so
//! `extend type Query { … }` grows the real `Query` node and new types link up
//! to existing ones for free. Two things concatenation can't express are
//! handled by rewriting the overlay first (see [`prepare_overlay`]):
//!
//!   - **Augmenting.** A bare `type User { … }` naming a type the schema
//!     already declares is turned into `extend type User { … }`, so you can
//!     paste a type straight out of the schema, add a field, and have it merge
//!     instead of collide.
//!   - **Removing.** SDL has no syntax for taking something away, so a line of
//!     the form `-name` is lifted out of the text and passed to the parser as a
//!     [`SchemaRemoval`] instead.
//!
//! Both read top to bottom: for any one name, the last line that mentions it
//! wins, so `-meh` above a `meh: Meh!` is just a rewrite while `-meh` below it
//! is a deletion.
//!
//! Restating a field the type already has is an override, not a clash — that's
//! [`SdlToGraphOptions::override_duplicates`](super::SdlToGraphOptions::override_duplicates),
//! so changing a field's type is a
//! matter of writing the field again with the type you want.
//!
//! The last thing concatenation loses is provenance — which nodes and fields
//! came from the overlay — which [`mark_overlay`] recovers by diffing the
//! merged graph against the base one.
//!
//! # Known divergence from the TS
//!
//! Two, both inherited rather than introduced:
//!
//! - [`declared_members`] parses with `async_graphql_parser` instead of
//!   graphql-js, so the handful of documents noted in [`super::sdl`] as
//!   parse errors there yield an empty map (nothing gets rewritten) where the
//!   TS would have had names to work with.
//! - [`mark_overlay`] takes `merged` by value and mutates only the nodes it
//!   marks. The TS relies on reference identity for the same guarantee
//!   ("untouched nodes are the very same object"); in Rust the observable
//!   property is that an untouched node compares equal to its input.

use std::collections::{HashMap, HashSet};

use super::types::{GraphNodeData, NodeKind, ParsedGraph, SchemaRemoval};
use async_graphql_parser::parse_schema;
use async_graphql_parser::types::{TypeKind, TypeSystemDefinition};

/// Joins base and overlay SDL into the single document that gets parsed.
pub fn merge_sdl(base: &str, overlay: &str) -> String {
    if overlay.trim().is_empty() {
        return base.to_string();
    }
    format!("{base}\n\n{overlay}")
}

/// What the overlay contributed, recovered by diffing merged against base.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OverlayDiff {
    /// Types present only in the merged graph.
    pub added_types: Vec<String>,
    /// `TypeName.fieldName` for rows added to a pre-existing type.
    pub added_fields: Vec<String>,
    /// `TypeName.fieldName` for rows the overlay restated with a different
    /// shape — a new type, different args, a changed deprecation.
    pub changed_fields: Vec<String>,
    /// Types the overlay took away with a top-level `-TypeName`.
    pub removed_types: Vec<String>,
    /// `TypeName.fieldName` for rows the overlay took away with `-name`.
    pub removed_fields: Vec<String>,
}

/// Base plus rewritten overlay, ready to hand to [`super::sdl_to_graph`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreparedOverlay {
    /// The document to parse.
    pub sdl: String,
    /// What the overlay's `-name` lines asked to delete, in source order.
    /// Pass as [`SdlToGraphOptions::remove`](super::SdlToGraphOptions::remove).
    pub removals: Vec<SchemaRemoval>,
    /// Problems in the overlay's own shorthand — a `-name` that couldn't be
    /// attached to a type. Whether a removal *resolves* is decided later, by
    /// the graph build.
    pub warnings: Vec<String>,
}

/// Member names of one declared type, in declaration order and deduplicated —
/// the Rust stand-in for the TS `Set`, whose iteration order the removal queue
/// depends on.
pub type MemberNames = Vec<String>;

/// What an SDL document declares: every type name mapped to its member names
/// (fields, enum values, union members). The keys are the set an overlay
/// augments rather than redefines; the values are what a `-Type` followed by a
/// fresh declaration has to clear out.
///
/// Extensions don't count — they name a type someone else declared.
/// Unparseable input yields an empty map, which simply means nothing gets
/// rewritten.
pub fn declared_members(sdl: &str) -> HashMap<String, MemberNames> {
    let mut members: HashMap<String, MemberNames> = HashMap::new();
    if sdl.trim().is_empty() {
        return members;
    }
    let Ok(doc) = parse_schema(sdl) else { return members };

    for def in &doc.definitions {
        let TypeSystemDefinition::Type(ty) = def else { continue };
        if ty.node.extend {
            continue;
        }
        let name = ty.node.name.node.as_str().to_string();
        let names: Vec<String> = match &ty.node.kind {
            TypeKind::Object(o) => o.fields.iter().map(|f| f.node.name.node.to_string()).collect(),
            TypeKind::Interface(i) => {
                i.fields.iter().map(|f| f.node.name.node.to_string()).collect()
            }
            TypeKind::InputObject(i) => {
                i.fields.iter().map(|f| f.node.name.node.to_string()).collect()
            }
            TypeKind::Enum(e) => e.values.iter().map(|v| v.node.value.node.to_string()).collect(),
            TypeKind::Union(u) => u.members.iter().map(|m| m.node.to_string()).collect(),
            TypeKind::Scalar => Vec::new(),
        };
        members.insert(name, dedup(names));
    }
    members
}

/// Type names an SDL document declares — see [`declared_members`].
pub fn declared_type_names(sdl: &str) -> HashSet<String> {
    declared_members(sdl).into_keys().collect()
}

fn dedup(names: Vec<String>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    names.into_iter().filter(|n| seen.insert(n.clone())).collect()
}

// ── line scanners (the TS regexes, hand-rolled) ────────────────────────────

/// End index (exclusive) of a GraphQL name starting at `i`, if there is one.
fn ident_end(chars: &[char], i: usize) -> Option<usize> {
    let c = *chars.get(i)?;
    if !(c == '_' || c.is_ascii_alphabetic()) {
        return None;
    }
    let mut j = i + 1;
    while j < chars.len() && (chars[j] == '_' || chars[j].is_ascii_alphanumeric()) {
        j += 1;
    }
    Some(j)
}

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Matches a removal line: `-email`, `- email`, or `-User.email`.
/// TS: `/^-\s*([_A-Za-z][_A-Za-z0-9]*)(?:\.([_A-Za-z][_A-Za-z0-9]*))?$/`
fn parse_remove_line(trimmed: &str) -> Option<(String, Option<String>)> {
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.first() != Some(&'-') {
        return None;
    }
    let i = skip_ws(&chars, 1);
    let end = ident_end(&chars, i)?;
    let first: String = chars[i..end].iter().collect();

    let mut i = end;
    let mut second = None;
    if chars.get(i) == Some(&'.') {
        let e2 = ident_end(&chars, i + 1)?;
        second = Some(chars[i + 1..e2].iter().collect::<String>());
        i = e2;
    }
    if i != chars.len() {
        return None;
    }
    Some((first, second))
}

const KEYWORDS: [&str; 6] = ["type", "interface", "input", "enum", "union", "scalar"];

/// Matches the head of a type-system definition or extension, returning
/// `(already_extend, keyword, name)`.
/// TS: `/^(extend\s+)?(type|interface|input|enum|union|scalar)\s+([_A-Za-z][_A-Za-z0-9]*)/`
fn parse_def_line(trimmed: &str) -> Option<(bool, &'static str, String)> {
    let chars: Vec<char> = trimmed.chars().collect();
    let mut i = 0;
    let mut already_extend = false;

    if word_at(&chars, 0, "extend") && chars.get(6).is_some_and(|c| c.is_whitespace()) {
        already_extend = true;
        i = skip_ws(&chars, 6);
    }

    let keyword = KEYWORDS.into_iter().find(|k| {
        word_at(&chars, i, k) && chars.get(i + k.len()).is_some_and(|c| c.is_whitespace())
    })?;
    let i = skip_ws(&chars, i + keyword.len());
    let end = ident_end(&chars, i)?;
    Some((already_extend, keyword, chars[i..end].iter().collect()))
}

fn word_at(chars: &[char], i: usize, word: &str) -> bool {
    word.chars().enumerate().all(|(k, c)| chars.get(i + k) == Some(&c))
}

/// Matches the start of a member declaration inside a block: a field
/// (`name:` / `name(`), or an enum value (bare, or with a directive).
/// TS: `/^([_A-Za-z][_A-Za-z0-9]*)\s*(?::|\(|@|$)/`
fn parse_member_line(trimmed: &str) -> Option<String> {
    let chars: Vec<char> = trimmed.chars().collect();
    let end = ident_end(&chars, 0)?;
    let name: String = chars[..end].iter().collect();
    let i = skip_ws(&chars, end);
    match chars.get(i) {
        None => Some(name),
        Some(':' | '(' | '@') => Some(name),
        _ => None,
    }
}

/// TS: `/^"([^"\\]|\\.)*"$/` — a one-line, non-block string description.
fn is_simple_string_literal(trimmed: &str) -> bool {
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() < 2 || chars[0] != '"' {
        return false;
    }
    let mut i = 1;
    while i < chars.len() {
        match chars[i] {
            '"' => return i == chars.len() - 1,
            '\\' if i + 1 < chars.len() => i += 2,
            '\\' => return false,
            _ => i += 1,
        }
    }
    false
}

// ── helpers ───────────────────────────────────────────────────────────────

fn is_triple(chars: &[char], i: usize) -> bool {
    i + 3 <= chars.len() && chars[i] == '"' && chars[i + 1] == '"' && chars[i + 2] == '"'
}

fn find_triple(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len()).find(|&i| is_triple(chars, i))
}

/// Strips comments and string bodies from one line so brace counting and
/// keyword matching only ever see code. `in_block` carries an open `"""`
/// across lines.
fn strip_literals(line: &str, in_block: &mut bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;

    while i < chars.len() {
        if *in_block {
            let Some(end) = find_triple(&chars, i) else { return out };
            *in_block = false;
            i = end + 3;
            continue;
        }
        let ch = chars[i];
        if ch == '#' {
            break;
        }
        if is_triple(&chars, i) {
            *in_block = true;
            i += 3;
            continue;
        }
        if ch == '"' {
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// Member names declared on a definition line that carries its body inline —
/// `extend type User { name: String! }`, `enum S { A, B }`. The split respects
/// nesting so a comma inside an argument list or a default value doesn't look
/// like the start of the next member.
fn inline_member_names(code: &str) -> Vec<String> {
    let Some(open) = code.find('{') else { return Vec::new() };
    let close = code.rfind('}');
    let body: Vec<char> = match close {
        Some(close) if close > open => code[open + 1..close].chars().collect(),
        _ => code[open + 1..].chars().collect(),
    };

    let mut parts: Vec<String> = Vec::new();
    let mut depth: usize = 0;
    let mut start = 0;
    for i in 0..body.len() {
        match body[i] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(body[start..i].iter().collect());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(body[start..].iter().collect());

    parts.iter().filter_map(|p| parse_member_line(p.trim())).collect()
}

/// Blanks the description immediately above line `i`, if there is one.
/// `extend type Foo` may not carry a description, so a pasted type that
/// brought its doc comment along would otherwise become a syntax error the
/// moment we rewrite it into an extension.
fn blank_preceding_description(lines: &mut [String], i: usize) {
    let mut j = i as isize - 1;
    while j >= 0 && lines[j as usize].trim().is_empty() {
        j -= 1;
    }
    if j < 0 {
        return;
    }
    let j = j as usize;
    let t = lines[j].trim().to_string();

    if t.ends_with("\"\"\"") {
        // A one-line `"""doc"""` opens and closes on the same line; anything
        // else means walking back to the opening delimiter.
        let mut k = j as isize;
        if !(t.starts_with("\"\"\"") && t.chars().count() >= 6) {
            k = j as isize - 1;
            while k >= 0 && !lines[k as usize].trim().starts_with("\"\"\"") {
                k -= 1;
            }
            if k < 0 {
                return;
            }
        }
        for line in &mut lines[k as usize..=j] {
            line.clear();
        }
        return;
    }
    if is_simple_string_literal(&t) {
        lines[j].clear();
    }
}

/// True when anything is left between a block's braces. Used to spot a block
/// that removals emptied out: `type User { -name }` leaves `{ }`, which SDL
/// rejects, so the block has to go rather than be handed to the parser.
fn block_has_members(lines: &[String], start: usize, end: usize) -> bool {
    let mut in_block = false;
    let mut stripped: Vec<String> = Vec::new();
    for line in &lines[start..=end] {
        stripped.push(strip_literals(line, &mut in_block));
    }
    let code = stripped.join("\n");

    let Some(open) = code.find('{') else { return true };
    let Some(close) = code.rfind('}') else { return true };
    if close < open {
        return true;
    }
    !code[open + 1..close].trim().is_empty()
}

/// The overlay's last word on a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Word {
    Define,
    Remove,
}

/// Rewrites overlay SDL into something parseable next to `base`: bare
/// redefinitions of known types become extensions, and `-name` lines are
/// lifted out into removals.
///
/// Lines are blanked rather than deleted so line numbers keep matching the
/// editor — a parse error still points at the line the user is looking at.
///
/// The overlay reads top to bottom, so for any one name the last line that
/// mentions it decides its fate:
///
/// ```text
/// type Foo {
///   meh: Meh!    # define
///   -meh         # …then drop it
///   meh: Meh!    # …then define it again — this is what stands
/// }
/// ```
///
/// Removals are applied after every extension is folded in (SDL has no way to
/// express them inline), so ordering can't come from position alone — it comes
/// from dropping the removals that a later line overrode.
pub fn rewrite_overlay(
    overlay: &str,
    base_members: &HashMap<String, MemberNames>,
) -> PreparedOverlay {
    let mut lines: Vec<String> = overlay.split('\n').map(str::to_string).collect();
    // Removals in source order, each tagged with the name it targets.
    let mut asked: Vec<(SchemaRemoval, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut in_block = false;
    // Brace nesting, counted over code only.
    let mut depth: usize = 0;
    // Paren nesting — an argument list spanning lines must not have its
    // arguments mistaken for the enclosing type's fields.
    let mut parens: usize = 0;
    // Type whose block we're inside, for bare `-name` lines.
    let mut current_type: Option<String> = None;
    // Line the innermost-enclosing top-level definition starts on.
    let mut def_line: isize = -1;
    // First line of the top-level block currently open, if any.
    let mut open_line: isize = -1;
    // Top-level `{ … }` spans, checked for emptiness once closed.
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    // The overlay's last word on a type (`Foo`) or member (`Foo.bar`).
    let mut last_word: HashMap<String, Word> = HashMap::new();

    for i in 0..lines.len() {
        let code = strip_literals(&lines[i], &mut in_block);
        let trimmed = code.trim();

        if let Some((first, second)) = parse_remove_line(trimmed) {
            let removal: Option<SchemaRemoval> = match second {
                Some(member) => Some(SchemaRemoval::member(&first, member)),
                None if depth == 0 => Some(SchemaRemoval::type_only(&first)),
                None => current_type.as_ref().map(|t| SchemaRemoval::member(t, &first)),
            };
            match removal {
                Some(removal) => {
                    let key = match &removal.member {
                        Some(m) => format!("{}.{}", removal.type_, m),
                        None => removal.type_.clone(),
                    };
                    last_word.insert(key.clone(), Word::Remove);
                    asked.push((removal, key));
                }
                None => warnings.push(format!(
                    "Ignoring \"-{}\" on line {} — not inside a type block; \
                     write \"-Type.{}\" to remove a field",
                    first,
                    i + 1,
                    first
                )),
            }
            lines[i].clear();
            continue;
        }

        if depth == 0 {
            if let Some((already_extend, keyword, name)) = parse_def_line(trimmed) {
                current_type = Some(name.clone());
                def_line = i as isize;
                last_word.insert(name.clone(), Word::Define);

                // A union declares its members inline, with no block to scan.
                if keyword == "union" {
                    if let Some(eq) = code.find('=') {
                        for m in code[eq + 1..].split('|') {
                            let t = m.trim();
                            if !t.is_empty() {
                                last_word.insert(format!("{name}.{t}"), Word::Define);
                            }
                        }
                    }
                }
                // Everything else may still open and close its body on this one
                // line, which the per-line member scan below would never see.
                for m in inline_member_names(&code) {
                    last_word.insert(format!("{name}.{m}"), Word::Define);
                }

                if !already_extend && base_members.contains_key(&name) {
                    // `extend scalar X` is only legal with directives attached,
                    // and a scalar redeclaration carries nothing to merge
                    // anyway — drop the line instead of extending it.
                    if keyword == "scalar" {
                        lines[i].clear();
                        continue;
                    }
                    // Unions have no braces; every other kind needs its body on
                    // this line for the extension to be well-formed. Anything
                    // else is left alone, so the duplicate is reported rather
                    // than silently mangled.
                    let extendable = if keyword == "union" {
                        code.contains('=')
                    } else {
                        code.contains('{')
                    };
                    if extendable {
                        blank_preceding_description(&mut lines, i);
                        let indent =
                            lines[i].find(|c: char| !c.is_whitespace()).unwrap_or(lines[i].len());
                        lines[i] = format!("{}extend {}", &lines[i][..indent], &lines[i][indent..]);
                    }
                }
            }
        }
        // A member declaration sits at brace depth 1 (deeper braces are default
        // values) and outside any argument list.
        else if depth == 1 && parens == 0 {
            if let Some(ty) = current_type.clone() {
                if let Some(member) = parse_member_line(trimmed) {
                    last_word.insert(format!("{ty}.{member}"), Word::Define);
                }
            }
        }

        for ch in code.chars() {
            match ch {
                '{' => {
                    if depth == 0 {
                        open_line = if def_line >= 0 { def_line } else { i as isize };
                    }
                    depth += 1;
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        if open_line >= 0 {
                            blocks.push((open_line as usize, i));
                        }
                        open_line = -1;
                        current_type = None;
                        def_line = -1;
                    }
                }
                '(' => parens += 1,
                ')' => parens = parens.saturating_sub(1),
                _ => {}
            }
        }
    }

    // A block whose every member was removed can't be parsed, so it goes away
    // entirely — along with any description that would otherwise dangle in
    // front of the next definition.
    for (start, end) in blocks {
        if block_has_members(&lines, start, end) {
            continue;
        }
        blank_preceding_description(&mut lines, start);
        for line in &mut lines[start..=end] {
            line.clear();
        }
    }

    // Ordering, resolved: a removal only survives if nothing below it put the
    // name back. Declaring the member again is the rewrite — the extension's
    // field then overrides whatever was there.
    let mut removals: Vec<SchemaRemoval> = asked
        .iter()
        .filter(|(_, key)| last_word.get(key) == Some(&Word::Remove))
        .map(|(removal, _)| removal.clone())
        .collect();

    // A type dropped and then declared again is a replacement rather than a
    // deletion: clear out the members the schema had, and let the block below
    // supply the type's new contents.
    for (removal, key) in &asked {
        if removal.member.is_some() || last_word.get(key) == Some(&Word::Remove) {
            continue;
        }
        let Some(members) = base_members.get(&removal.type_) else { continue };
        for m in members {
            if last_word.get(&format!("{}.{}", removal.type_, m)) == Some(&Word::Define) {
                continue;
            }
            removals.push(SchemaRemoval::member(&removal.type_, m));
        }
    }

    PreparedOverlay { sdl: lines.join("\n"), removals, warnings }
}

/// Builds the document to parse from a schema and the SDL laid over it.
pub fn prepare_overlay(base: &str, overlay: &str) -> PreparedOverlay {
    if overlay.trim().is_empty() {
        return PreparedOverlay {
            sdl: base.to_string(),
            removals: Vec::new(),
            warnings: Vec::new(),
        };
    }
    let rewritten = rewrite_overlay(overlay, &declared_members(base));
    PreparedOverlay {
        sdl: merge_sdl(base, &rewritten.sdl),
        removals: rewritten.removals,
        warnings: rewritten.warnings,
    }
}

// ── provenance ────────────────────────────────────────────────────────────

/// Row names (fields for most kinds, values for enums, members for unions).
fn row_names(n: &GraphNodeData) -> Vec<String> {
    match n.kind {
        NodeKind::Enum => n.values.iter().flatten().map(|v| v.name.clone()).collect(),
        NodeKind::Union => n.members.clone().unwrap_or_default(),
        _ => n.fields.iter().flatten().map(|f| f.name.clone()).collect(),
    }
}

/// Everything about a row that the card renders, minus its name. Comparing
/// these is how an override is spotted: the row is neither added nor removed,
/// but it isn't what the schema had either. Union members carry nothing beyond
/// their name, so they never register as changed.
fn row_shapes(n: &GraphNodeData) -> HashMap<String, String> {
    let mut shapes = HashMap::new();
    match n.kind {
        NodeKind::Union => {}
        NodeKind::Enum => {
            for v in n.values.iter().flatten() {
                shapes.insert(
                    v.name.clone(),
                    format!(
                        "{}{}|{}",
                        if v.is_deprecated { "!" } else { "" },
                        v.deprecation_reason.as_deref().unwrap_or(""),
                        v.description.as_deref().unwrap_or(""),
                    ),
                );
            }
        }
        _ => {
            for f in n.fields.iter().flatten() {
                let args = f
                    .args
                    .iter()
                    .flatten()
                    .map(|a| format!("{}:{}", a.name, a.type_))
                    .collect::<Vec<_>>()
                    .join(",");
                shapes.insert(
                    f.name.clone(),
                    format!(
                        "{}({})|{}{}|{}",
                        f.type_,
                        args,
                        if f.is_deprecated { "!" } else { "" },
                        f.deprecation_reason.as_deref().unwrap_or(""),
                        f.description.as_deref().unwrap_or(""),
                    ),
                );
            }
        }
    }
    shapes
}

/// Marks everything the overlay contributed. Returns the merged graph with
/// `is_overlay` set on its new nodes, and on just the rows the overlay added
/// to pre-existing nodes — plus a plain summary for the panel to display.
///
/// Removals show up in the summary only: a row that's gone has nothing left to
/// paint. They're recovered by diffing in the other direction — anything in the
/// base graph the merged graph no longer has.
///
/// Only nodes that actually changed are touched, so an overlay affecting one
/// type doesn't churn every node (which would reset the canvas texture cache
/// for the whole graph).
pub fn mark_overlay(base: &ParsedGraph, merged: ParsedGraph) -> (ParsedGraph, OverlayDiff) {
    let mut base_rows: HashMap<&str, HashSet<String>> = HashMap::new();
    let mut base_shapes: HashMap<&str, HashMap<String, String>> = HashMap::new();
    for n in &base.nodes {
        base_rows.insert(n.id.as_str(), row_names(n).into_iter().collect());
        base_shapes.insert(n.id.as_str(), row_shapes(n));
    }

    let mut diff = OverlayDiff::default();
    let mut merged = merged;

    // Removals first, while `merged` is still untouched — marking never changes
    // a node's name or row list, so the two passes are independent.
    {
        let merged_by_id: HashMap<&str, &GraphNodeData> =
            merged.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        for b in &base.nodes {
            let Some(m) = merged_by_id.get(b.id.as_str()) else {
                diff.removed_types.push(b.name.clone());
                continue;
            };
            let kept: HashSet<String> = row_names(m).into_iter().collect();
            for r in row_names(b) {
                if !kept.contains(&r) {
                    diff.removed_fields.push(format!("{}.{}", b.name, r));
                }
            }
        }
    }

    let empty_shapes: HashMap<String, String> = HashMap::new();
    for n in &mut merged.nodes {
        let Some(prior) = base_rows.get(n.id.as_str()) else {
            diff.added_types.push(n.name.clone());
            n.is_overlay = true;
            continue;
        };

        // Added and overridden rows are both "the overlay's work" as far as the
        // canvas is concerned — one emerald marker covers both — but the diff
        // keeps them apart so the panel can say which is which.
        let prior_shapes = base_shapes.get(n.id.as_str()).unwrap_or(&empty_shapes);
        let shapes = row_shapes(n);
        let is_new = |name: &str| !prior.contains(name);
        let is_changed =
            |name: &str| !is_new(name) && shapes.get(name) != prior_shapes.get(name);
        let touched = |name: &str| is_new(name) || is_changed(name);

        if !row_names(n).iter().any(|r| touched(r)) {
            continue;
        }
        let node_name = n.name.clone();

        if n.kind == NodeKind::Enum {
            let values = n.values.get_or_insert_with(Vec::new);
            for v in values.iter_mut() {
                if touched(&v.name) {
                    v.is_overlay = true;
                }
            }
            for v in values.iter() {
                if !v.is_overlay {
                    continue;
                }
                let row = format!("{}.{}", node_name, v.name);
                if is_new(&v.name) {
                    diff.added_fields.push(row);
                } else {
                    diff.changed_fields.push(row);
                }
            }
            continue;
        }
        if n.kind == NodeKind::Union {
            // Union members render as plain rows with no per-row metadata, so
            // there is nothing to flag on the node itself — the new member type
            // gets flagged in its own right when it is also new.
            for m in n.members.iter().flatten() {
                if is_new(m) {
                    diff.added_fields.push(format!("{node_name}.{m}"));
                }
            }
            continue;
        }

        let fields = n.fields.get_or_insert_with(Vec::new);
        for f in fields.iter_mut() {
            if touched(&f.name) {
                f.is_overlay = true;
            }
        }
        for f in fields.iter() {
            if !f.is_overlay {
                continue;
            }
            let row = format!("{}.{}", node_name, f.name);
            if is_new(&f.name) {
                diff.added_fields.push(row);
            } else {
                diff.changed_fields.push(row);
            }
        }
    }

    diff.added_types.sort();
    diff.added_fields.sort();
    diff.changed_fields.sort();
    diff.removed_types.sort();
    diff.removed_fields.sort();
    (merged, diff)
}

#[cfg(test)]
mod tests;
