//! Ported from `src/lib/overlay.test.ts`, plus coverage for the orderings and
//! line-scrubbing corners that suite exercised only implicitly.

use super::*;
use crate::graph::{sdl_to_graph, GraphNodeData, ParsedGraph, SchemaRemoval, SdlToGraphOptions};

const BASE: &str = "
  type Query { user(id: ID!): User }
  type User { id: ID!, name: String }
  enum Status { ACTIVE }
";

struct Applied {
    graph: ParsedGraph,
    diff: OverlayDiff,
    warnings: Vec<String>,
}

/// Parses base and base+overlay the way the schema context does.
fn apply(overlay: &str) -> Applied {
    apply_to(BASE, overlay)
}

fn apply_to(base_sdl: &str, overlay: &str) -> Applied {
    let base = sdl_to_graph(base_sdl, &SdlToGraphOptions::default());
    let prepared = prepare_overlay(base_sdl, overlay);
    let merged = sdl_to_graph(
        &prepared.sdl,
        &SdlToGraphOptions {
            remove: prepared.removals.clone(),
            override_duplicates: true,
            ..SdlToGraphOptions::default()
        },
    );
    assert!(merged.error.is_none(), "{:?}", merged.error);

    let mut warnings = prepared.warnings.clone();
    warnings.extend(
        merged.warnings.iter().filter(|w| !base.warnings.contains(w)).cloned(),
    );
    let (graph, diff) = mark_overlay(&base, merged);
    Applied { graph, diff, warnings }
}

fn node<'a>(g: &'a ParsedGraph, name: &str) -> &'a GraphNodeData {
    g.nodes.iter().find(|n| n.name == name).unwrap_or_else(|| panic!("no node {name}"))
}

fn field_names(n: &GraphNodeData) -> Vec<&str> {
    n.fields.as_ref().unwrap().iter().map(|f| f.name.as_str()).collect()
}

fn field_types(n: &GraphNodeData) -> Vec<(&str, &str)> {
    n.fields.as_ref().unwrap().iter().map(|f| (f.name.as_str(), f.type_.as_str())).collect()
}

fn value_names(n: &GraphNodeData) -> Vec<&str> {
    n.values.as_ref().unwrap().iter().map(|v| v.name.as_str()).collect()
}

fn field_overlay(n: &GraphNodeData, name: &str) -> bool {
    n.fields.as_ref().unwrap().iter().find(|f| f.name == name).unwrap().is_overlay
}

fn empty() -> Vec<String> {
    Vec::new()
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ── merging ────────────────────────────────────────────────────────────────

#[test]
fn merge_sdl_leaves_the_base_untouched_when_the_overlay_is_blank() {
    assert_eq!(merge_sdl("type Query { a: String }", "   \n "), "type Query { a: String }");
}

#[test]
fn merge_sdl_joins_with_a_blank_line() {
    assert_eq!(merge_sdl("type Query { a: String }", "scalar X"), "type Query { a: String }\n\nscalar X");
}

#[test]
fn a_brand_new_type_is_flagged_on_the_node() {
    let a = apply(
        "
    extend type Query { report: Report }
    type Report { id: ID! }
  ",
    );
    assert!(node(&a.graph, "Report").is_overlay);
    assert_eq!(a.diff.added_types, v(&["Report"]));
}

#[test]
fn a_field_added_to_an_existing_type_is_flagged_on_the_row_not_the_node() {
    let a = apply("extend type User { email: String }");
    let user = node(&a.graph, "User");
    assert!(!user.is_overlay);
    assert!(field_overlay(user, "email"));
    assert!(!field_overlay(user, "id"));
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
    assert_eq!(a.diff.added_types, empty());
}

#[test]
fn enum_values_added_by_an_extension_are_flagged() {
    let a = apply("extend enum Status { ARCHIVED }");
    let status = node(&a.graph, "Status");
    let flags: Vec<(&str, bool)> =
        status.values.as_ref().unwrap().iter().map(|x| (x.name.as_str(), x.is_overlay)).collect();
    assert_eq!(flags, vec![("ACTIVE", false), ("ARCHIVED", true)]);
    assert_eq!(a.diff.added_fields, v(&["Status.ARCHIVED"]));
}

#[test]
fn untouched_nodes_keep_their_identity_so_the_canvas_cache_survives() {
    let base = sdl_to_graph(BASE, &SdlToGraphOptions::default());
    let merged = sdl_to_graph(
        &merge_sdl(BASE, "extend type User { email: String }"),
        &SdlToGraphOptions::default(),
    );
    let before = merged.clone();
    let (graph, _) = mark_overlay(&base, merged);
    // Query gained nothing — mark_overlay must hand it back unchanged.
    assert_eq!(node(&graph, "Query"), node(&before, "Query"));
    // …and the edge list rides along untouched too.
    assert_eq!(graph.edges, before.edges);
}

#[test]
fn an_overlay_restating_a_field_verbatim_reports_an_empty_diff() {
    let a = apply("extend type User { name: String }");
    assert_eq!(a.diff, OverlayDiff::default());
}

// ── overriding: restating a field replaces it ──────────────────────────────

#[test]
fn restating_a_field_with_a_different_type_overrides_it_in_place() {
    let a = apply(
        "
    type User {
      name: String!
    }
  ",
    );
    // Same position, new type — the card doesn't reshuffle.
    assert_eq!(field_types(node(&a.graph, "User")), vec![("id", "ID!"), ("name", "String!")]);
    assert_eq!(a.diff.changed_fields, v(&["User.name"]));
    assert_eq!(a.diff.added_fields, empty());
    assert_eq!(a.warnings, empty());
}

#[test]
fn an_overridden_row_is_flagged_like_an_added_one_so_the_canvas_marks_it() {
    let a = apply("type User { name: String! }");
    let user = node(&a.graph, "User");
    assert!(field_overlay(user, "name"));
    assert!(!field_overlay(user, "id"));
}

#[test]
fn overriding_a_fields_type_repoints_its_edges() {
    let a = apply(
        "
    type Report { id: ID! }
    type Query { user(id: ID!): Report }
  ",
    );
    let ids: Vec<&str> = a.graph.edges.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"Query.user->Report"), "{ids:?}");
    assert!(!ids.contains(&"Query.user->User"), "{ids:?}");
}

#[test]
fn overriding_an_argument_list_counts_as_a_change() {
    let a = apply("type Query { user(id: ID!, slug: String): User }");
    assert_eq!(a.diff.changed_fields, v(&["Query.user"]));
}

#[test]
fn overriding_an_enum_values_deprecation_counts_as_a_change() {
    let a = apply(
        "
    enum Status {
      ACTIVE @deprecated(reason: \"[until 2031-01-01] gone\")
    }
  ",
    );
    let status = node(&a.graph, "Status");
    let active = status.values.as_ref().unwrap().iter().find(|x| x.name == "ACTIVE").unwrap();
    assert_eq!(active.until.as_deref(), Some("2031-01-01"));
    assert!(active.is_overlay);
    assert_eq!(a.diff.changed_fields, v(&["Status.ACTIVE"]));
}

#[test]
fn a_changed_description_alone_counts_as_a_change() {
    let a = apply(
        "
    type User {
      \"\"\"Who they are.\"\"\"
      name: String
    }
  ",
    );
    assert_eq!(a.diff.changed_fields, v(&["User.name"]));
    assert_eq!(a.diff.added_fields, empty());
}

// ── augmenting: redeclaring a known type merges instead of colliding ───────

#[test]
fn declared_type_names_lists_definitions_but_not_extensions() {
    let mut names: Vec<String> = declared_type_names(
        "
    type Query { a: String }
    enum Status { ACTIVE }
    extend type Query { b: String }
    extend type Ghost { c: String }
  ",
    )
    .into_iter()
    .collect();
    names.sort();
    assert_eq!(names, v(&["Query", "Status"]));
}

#[test]
fn declared_members_lists_every_kinds_members() {
    let members = declared_members(
        "
    type Query { a: String, b: Int }
    interface Named { name: String }
    input Filter { q: String }
    enum Status { ACTIVE, GONE }
    union Thing = Query | Named
    scalar Date
  ",
    );
    assert_eq!(members["Query"], v(&["a", "b"]));
    assert_eq!(members["Named"], v(&["name"]));
    assert_eq!(members["Filter"], v(&["q"]));
    assert_eq!(members["Status"], v(&["ACTIVE", "GONE"]));
    assert_eq!(members["Thing"], v(&["Query", "Named"]));
    assert_eq!(members["Date"], empty());
}

#[test]
fn declared_members_of_unparseable_input_is_empty() {
    assert!(declared_members("type {{{").is_empty());
    assert!(declared_members("   ").is_empty());
}

#[test]
fn a_bare_redeclaration_of_an_existing_type_augments_it() {
    let a = apply(
        "
    type User { email: String }
  ",
    );
    let user = node(&a.graph, "User");
    assert_eq!(field_names(user), vec!["id", "name", "email"]);
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
    assert_eq!(a.warnings, empty());
    // One node, not two — the rewrite is what keeps the ids unique.
    assert_eq!(a.graph.nodes.iter().filter(|n| n.name == "User").count(), 1);
}

#[test]
fn augmenting_also_works_for_enums_and_a_type_the_schema_lacks_stays_new() {
    let a = apply(
        "
    enum Status { ARCHIVED }
    type Report { id: ID! }
  ",
    );
    assert_eq!(value_names(node(&a.graph, "Status")), vec!["ACTIVE", "ARCHIVED"]);
    assert_eq!(a.diff.added_types, v(&["Report"]));
    assert_eq!(a.diff.added_fields, v(&["Status.ARCHIVED"]));
}

#[test]
fn a_redeclaration_carrying_its_own_description_still_parses() {
    // `extend type User` may not take a description, so the rewrite has to
    // drop the doc block a pasted type brought with it.
    let a = apply(
        "
    \"\"\"A person.\"\"\"
    type User {
      email: String
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "name", "email"]);
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
}

#[test]
fn a_redeclaration_carrying_a_multiline_block_description_still_parses() {
    let a = apply(
        "
    \"\"\"
    A person.

    With a blank line in the middle.
    \"\"\"
    type User {
      email: String
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "name", "email"]);
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
}

#[test]
fn a_redeclaration_carrying_a_single_quoted_description_still_parses() {
    let a = apply(
        "
    \"A person.\"
    type User {
      email: String
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "name", "email"]);
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
}

#[test]
fn a_blank_line_between_the_description_and_the_type_is_walked_over() {
    let a = apply(
        "
    \"\"\"A person.\"\"\"

    type User {
      email: String
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "name", "email"]);
}

#[test]
fn an_already_written_extension_is_left_alone() {
    let prepared = prepare_overlay(BASE, "extend type User { email: String }");
    assert!(!prepared.sdl.contains("extend extend"), "{}", prepared.sdl);
}

#[test]
fn a_multiline_redeclaration_that_never_opens_a_block_is_left_to_collide() {
    // No `{` on the head line — the extension would be malformed, so the
    // duplicate is reported rather than silently mangled.
    let prepared = prepare_overlay(BASE, "type User\n{\n  email: String\n}");
    assert!(!prepared.sdl.contains("extend type User"), "{}", prepared.sdl);
}

#[test]
fn redeclaring_a_known_scalar_drops_the_line_entirely() {
    let base = "scalar Date\ntype Query { at: Date }";
    let prepared = prepare_overlay(base, "scalar Date");
    // The whole overlay blanked out, so merge_sdl hands back the base.
    assert_eq!(prepared.sdl, base);
    assert_eq!(prepared.removals, Vec::<SchemaRemoval>::new());
}

#[test]
fn a_union_redeclaration_is_rewritten_into_an_extension() {
    let base = "
    type Query { t: Thing }
    type A { x: Int }
    type B { x: Int }
    type C { x: Int }
    union Thing = A | B
  ";
    let a = apply_to(base, "union Thing = C");
    assert_eq!(node(&a.graph, "Thing").members.as_ref().unwrap(), &v(&["A", "B", "C"]));
    assert_eq!(a.diff.added_fields, v(&["Thing.C"]));
    assert_eq!(a.warnings, empty());
}

#[test]
fn a_union_member_is_removable() {
    let base = "
    type Query { t: Thing }
    type A { x: Int }
    type B { x: Int }
    union Thing = A | B
  ";
    let a = apply_to(base, "-Thing.A");
    assert_eq!(node(&a.graph, "Thing").members.as_ref().unwrap(), &v(&["B"]));
    assert_eq!(a.diff.removed_fields, v(&["Thing.A"]));
}

// ── removing: `-name` takes something away ─────────────────────────────────

#[test]
fn a_dash_field_inside_a_block_removes_that_field() {
    let a = apply(
        "
    type User {
      -name
      email: String
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "email"]);
    assert_eq!(a.diff.removed_fields, v(&["User.name"]));
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
    assert_eq!(a.warnings, empty());
}

#[test]
fn removing_a_field_drops_the_edges_it_carried() {
    let a = apply(
        "
    type Query {
      -user
    }
  ",
    );
    assert!(node(&a.graph, "Query").fields.as_ref().unwrap().is_empty());
    let ids: Vec<&str> = a.graph.edges.iter().map(|e| e.id.as_str()).collect();
    assert!(!ids.contains(&"Query.user->User"), "{ids:?}");
}

#[test]
fn a_block_emptied_by_removals_is_dropped_description_and_all() {
    // `extend type Query { }` is a syntax error, so the whole block — plus the
    // doc comment that would then dangle — has to come out.
    let a = apply(
        "
    \"\"\"Root.\"\"\"
    type Query {
      -user
    }
  ",
    );
    assert!(node(&a.graph, "Query").fields.as_ref().unwrap().is_empty());
    assert_eq!(a.diff.removed_fields, v(&["Query.user"]));
    assert_eq!(a.warnings, empty());
}

#[test]
fn an_emptied_block_leaves_the_definition_that_follows_it_alone() {
    let a = apply(
        "
    \"\"\"Root.\"\"\"
    type Query {
      -user
    }
    type Report { id: ID! }
  ",
    );
    assert!(node(&a.graph, "Query").fields.as_ref().unwrap().is_empty());
    assert_eq!(a.diff.added_types, v(&["Report"]));
    assert_eq!(a.diff.removed_fields, v(&["Query.user"]));
}

#[test]
fn a_block_left_with_one_member_survives() {
    let a = apply(
        "
    type User {
      -name
      -id
      email: String
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["email"]);
    let mut removed = a.diff.removed_fields.clone();
    removed.sort();
    assert_eq!(removed, v(&["User.id", "User.name"]));
}

#[test]
fn a_dash_type_dot_field_works_from_the_top_level() {
    let a = apply("-User.name");
    assert_eq!(a.diff.removed_fields, v(&["User.name"]));
    assert_eq!(a.diff.removed_types, empty());
}

#[test]
fn a_bare_dash_type_at_the_top_level_removes_the_whole_type() {
    let a = apply("-Status");
    assert!(!a.graph.nodes.iter().any(|n| n.name == "Status"));
    assert_eq!(a.diff.removed_types, v(&["Status"]));
    assert_eq!(a.diff.removed_fields, empty());
}

#[test]
fn a_removal_line_tolerates_a_space_after_the_dash() {
    let a = apply("- Status");
    assert_eq!(a.diff.removed_types, v(&["Status"]));
}

#[test]
fn enum_values_are_removable_too() {
    let a = apply(
        "
    enum Status {
      -ACTIVE
      ARCHIVED
    }
  ",
    );
    assert_eq!(value_names(node(&a.graph, "Status")), vec!["ARCHIVED"]);
    assert_eq!(a.diff.removed_fields, v(&["Status.ACTIVE"]));
}

#[test]
fn a_removal_that_matches_nothing_warns_and_changes_nothing_else() {
    let a = apply(
        "
    type User {
      -nope
      email: String
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "name", "email"]);
    assert_eq!(a.diff.removed_fields, empty());
    assert_eq!(a.warnings, v(&["Cannot remove \"User.nope\" — no such field"]));
}

#[test]
fn a_bare_removal_with_no_enclosing_type_warns_instead() {
    // `schema {` opens a block that isn't a type definition, so there is no
    // name to hang the removal on.
    let prepared = prepare_overlay(BASE, "schema {\n  -foo\n}");
    assert_eq!(
        prepared.warnings,
        v(&["Ignoring \"-foo\" on line 2 — not inside a type block; write \"-Type.foo\" to remove a field"])
    );
    assert_eq!(prepared.removals, Vec::<SchemaRemoval>::new());
}

// ── ordering: the overlay reads top to bottom ──────────────────────────────

#[test]
fn a_removal_below_a_definition_of_the_same_name_wins() {
    let a = apply(
        "
    type User {
      email: String
      -email
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "name"]);
    assert_eq!(a.diff.added_fields, empty());
    assert_eq!(a.diff.removed_fields, empty());
}

#[test]
fn a_definition_below_a_removal_of_the_same_name_wins() {
    let a = apply(
        "
    type User {
      email: Email!
      -email
      email: Email!
    }
    scalar Email
  ",
    );
    assert_eq!(
        field_types(node(&a.graph, "User")),
        vec![("id", "ID!"), ("name", "String"), ("email", "Email!")]
    );
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
    assert_eq!(a.diff.removed_fields, empty());
    assert_eq!(a.warnings, empty());
}

#[test]
fn re_declaring_a_base_field_below_its_removal_overrides_instead() {
    let a = apply(
        "
    type User {
      -name
      name: String!
    }
  ",
    );
    assert_eq!(field_types(node(&a.graph, "User")), vec![("id", "ID!"), ("name", "String!")]);
    assert_eq!(a.diff.changed_fields, v(&["User.name"]));
    assert_eq!(a.diff.removed_fields, empty());
}

#[test]
fn re_declaring_a_base_field_above_its_removal_still_deletes_it() {
    let a = apply(
        "
    type User {
      name: String!
      -name
    }
  ",
    );
    assert_eq!(field_types(node(&a.graph, "User")), vec![("id", "ID!")]);
    assert_eq!(a.diff.removed_fields, v(&["User.name"]));
    assert_eq!(a.diff.changed_fields, empty());
}

#[test]
fn ordering_holds_across_blocks_and_for_a_dash_type_dot_field_above_one() {
    let a = apply(
        "
    -User.name
    extend type User { name: String! }
  ",
    );
    let types: Vec<&str> =
        node(&a.graph, "User").fields.as_ref().unwrap().iter().map(|f| f.type_.as_str()).collect();
    assert_eq!(types, vec!["ID!", "String!"]);
    assert_eq!(a.diff.changed_fields, v(&["User.name"]));
    assert_eq!(a.diff.removed_fields, empty());
}

#[test]
fn a_dash_type_dot_field_below_a_block_that_defines_it_still_deletes_it() {
    let a = apply(
        "
    extend type User { name: String! }
    -User.name
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id"]);
    assert_eq!(a.diff.removed_fields, v(&["User.name"]));
}

#[test]
fn a_type_dropped_and_then_declared_again_is_replaced_not_deleted() {
    let a = apply(
        "
    -User
    type User {
      name: String!
      handle: String!
    }
  ",
    );
    // `id` was the schema's and nothing below asked for it back; `name` was
    // re-declared, so it stays (with the overlay's type).
    assert_eq!(
        field_types(node(&a.graph, "User")),
        vec![("name", "String!"), ("handle", "String!")]
    );
    assert_eq!(a.diff.removed_types, empty());
    assert_eq!(a.diff.removed_fields, v(&["User.id"]));
    assert_eq!(a.diff.added_fields, v(&["User.handle"]));
    assert_eq!(a.diff.changed_fields, v(&["User.name"]));
}

#[test]
fn a_type_dropped_and_redeclared_inline_is_replaced_too() {
    // The inline body never reaches the per-line member scan, so
    // `inline_member_names` is what keeps `name` alive.
    let a = apply("-User\ntype User { name: String!, handle: String! }");
    assert_eq!(
        field_types(node(&a.graph, "User")),
        vec![("name", "String!"), ("handle", "String!")]
    );
    assert_eq!(a.diff.removed_fields, v(&["User.id"]));
}

#[test]
fn an_enum_dropped_and_redeclared_inline_keeps_the_values_it_restates() {
    let a = apply("-Status\nenum Status { ACTIVE, ARCHIVED }");
    assert_eq!(value_names(node(&a.graph, "Status")), vec!["ACTIVE", "ARCHIVED"]);
    assert_eq!(a.diff.removed_types, empty());
    assert_eq!(a.diff.removed_fields, empty());
    assert_eq!(a.diff.added_fields, v(&["Status.ARCHIVED"]));
}

#[test]
fn a_type_declared_and_then_dropped_is_still_gone() {
    let a = apply(
        "
    type User { handle: String! }
    -User
  ",
    );
    assert!(!a.graph.nodes.iter().any(|n| n.name == "User"));
    assert_eq!(a.diff.removed_types, v(&["User"]));
}

#[test]
fn an_argument_sharing_a_fields_name_doesnt_count_as_a_definition() {
    // `id` inside the arg list must not cancel the `-id` above it.
    let a = apply(
        "
    type Query {
      user(
        id: ID!
      ): User
    }
    type User {
      -id
    }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["name"]);
    assert_eq!(a.diff.removed_fields, v(&["User.id"]));
}

#[test]
fn a_nested_default_value_brace_doesnt_close_the_block() {
    let a = apply(
        "
    input Filter {
      where: JSON = { a: 1 }
      q: String
    }
    scalar JSON
    extend type Query { search(f: Filter): User }
  ",
    );
    assert_eq!(field_names(node(&a.graph, "Filter")), vec!["where", "q"]);
    assert_eq!(a.diff.added_types, v(&["Filter", "JSON"]));
}

// ── comments and strings are not code ──────────────────────────────────────

#[test]
fn a_removal_line_inside_a_comment_or_string_is_not_one() {
    let a = apply(
        "
    type User {
      \"\"\"
      -name
      \"\"\"
      email: String
      # -id
    }
  ",
    );
    assert_eq!(a.diff.removed_fields, empty());
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
    assert_eq!(a.warnings, empty());
}

#[test]
fn a_brace_inside_a_block_string_doesnt_open_a_block() {
    let a = apply(
        "
    type User {
      \"\"\"
      a stray } and {
      \"\"\"
      email: String
    }
    -User.name
  ",
    );
    assert_eq!(field_names(node(&a.graph, "User")), vec!["id", "email"]);
    assert_eq!(a.diff.removed_fields, v(&["User.name"]));
}

#[test]
fn a_removal_word_inside_a_single_quoted_string_is_not_one() {
    let a = apply(
        "
    type User {
      email: String @deprecated(reason: \"-name is gone\")
    }
  ",
    );
    assert_eq!(a.diff.removed_fields, empty());
    assert_eq!(a.diff.added_fields, v(&["User.email"]));
}

// ── line numbers survive the rewrite ───────────────────────────────────────

#[test]
fn blanked_lines_keep_the_parse_error_pointing_at_the_right_line() {
    let prepared = prepare_overlay(BASE, "-User.name\n\ntype Report { id: ID! }");
    let overlay_lines: Vec<&str> = prepared.sdl[BASE.len()..].split('\n').collect();
    // The `-User.name` line is blanked, not deleted, so what follows keeps its
    // position in the document.
    assert_eq!(overlay_lines.iter().filter(|l| l.contains("type Report")).count(), 1);
    assert_eq!(
        prepared.sdl.split('\n').count(),
        BASE.split('\n').count() + 1 + 3
    );
}

#[test]
fn rewriting_never_changes_the_overlays_line_count() {
    let overlay = "\"\"\"Root.\"\"\"\ntype Query {\n  -user\n}\n\n-Status\ntype User { email: String }";
    let rewritten = rewrite_overlay(overlay, &declared_members(BASE));
    assert_eq!(rewritten.sdl.split('\n').count(), overlay.split('\n').count());
}

#[test]
fn the_rewrite_preserves_indentation_when_it_inserts_extend() {
    let rewritten = rewrite_overlay("      type User { email: String }", &declared_members(BASE));
    assert_eq!(rewritten.sdl, "      extend type User { email: String }");
}
