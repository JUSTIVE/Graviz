//! Ported from the bun suites in `src/lib/*.test.ts` — chiefly
//! `extend-types.test.ts`, `root-types.test.ts`, `union-membership.test.ts`,
//! and the `sdl-to-graph` half of `overlay.test.ts` — plus coverage for the
//! stages those suites exercised only indirectly (Relay unwrapping, argument
//! chains, duplicate detection).

use std::collections::HashSet;

use super::*;

fn graph(sdl: &str) -> ParsedGraph {
    sdl_to_graph(sdl, &SdlToGraphOptions::default())
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

fn edge_ids(g: &ParsedGraph) -> Vec<&str> {
    g.edges.iter().map(|e| e.id.as_str()).collect()
}

fn field<'a>(n: &'a GraphNodeData, name: &str) -> &'a GraphField {
    n.fields.as_ref().unwrap().iter().find(|f| f.name == name).unwrap()
}

// ── extend-types.test.ts ───────────────────────────────────────────────────

#[test]
fn extend_type_appends_fields_to_the_existing_node() {
    let g = graph(
        r#"
        type Query { user(id: ID!): User }
        type User { id: ID! }
        extend type Query { report: Report }
        type Report { id: ID!, owner: User! }
        "#,
    );
    assert_eq!(field_names(node(&g, "Query")), vec!["user", "report"]);
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
}

#[test]
fn fields_added_by_an_extension_produce_graph_edges() {
    let g = graph(
        r#"
        type Query { user(id: ID!): User }
        type User { id: ID! }
        extend type Query { report: Report }
        type Report { owner: User! }
        "#,
    );
    let ids = edge_ids(&g);
    assert!(ids.contains(&"Query.report->Report"), "{ids:?}");
    assert!(ids.contains(&"Report.owner->User"), "{ids:?}");
}

#[test]
fn extension_fields_keep_their_deprecation_metadata() {
    let g = graph(
        r#"
        type Query { me: User }
        type User { id: ID! }
        extend type User {
          legacyName: String @deprecated(reason: "[until 2030-01-01] use fullName")
        }
        "#,
    );
    let f = field(node(&g, "User"), "legacyName");
    assert!(f.is_deprecated);
    assert_eq!(f.until.as_deref(), Some("2030-01-01"));
    assert_eq!(f.deprecation_reason.as_deref(), Some("[until 2030-01-01] use fullName"));
}

#[test]
fn extend_enum_union_interface_merge_into_their_targets() {
    let g = graph(
        r#"
        type Query { thing: Thing }
        enum Status { ACTIVE }
        extend enum Status { ARCHIVED }
        union Thing = Post
        type Post implements Timestamped { id: ID! }
        extend union Thing = Comment
        type Comment { id: ID! }
        interface Timestamped { createdAt: String }
        extend interface Timestamped { updatedAt: String }
        "#,
    );
    assert_eq!(value_names(node(&g, "Status")), vec!["ACTIVE", "ARCHIVED"]);
    assert_eq!(node(&g, "Thing").members.as_ref().unwrap(), &["Post", "Comment"]);
    assert_eq!(field_names(node(&g, "Timestamped")), vec!["createdAt", "updatedAt"]);
}

#[test]
fn extend_type_can_add_an_interface_implementation() {
    let g = graph(
        r#"
        type Query { post: Post }
        interface Timestamped { createdAt: String }
        type Post { id: ID! }
        extend type Post implements Timestamped { createdAt: String }
        "#,
    );
    assert_eq!(node(&g, "Post").interfaces.as_ref().unwrap(), &["Timestamped"]);
    assert!(edge_ids(&g).contains(&"Timestamped-impl-Post"));
}

#[test]
fn extending_an_unknown_type_warns_instead_of_throwing() {
    let g = graph(
        r#"
        type Query { me: User }
        type User { id: ID! }
        extend type Ghost { boo: String }
        "#,
    );
    assert_eq!(g.error, None);
    assert!(!g.nodes.iter().any(|n| n.name == "Ghost"));
    assert!(
        g.warnings.iter().any(|w| w.contains("Cannot extend unknown type \"Ghost\"")),
        "{:?}",
        g.warnings
    );
}

#[test]
fn kind_mismatch_and_duplicate_members_are_warned_and_skipped() {
    let g = graph(
        r#"
        type Query { me: User }
        interface User { id: ID! }
        extend type User { name: String }
        type Account { email: String }
        extend type Account { email: String }
        "#,
    );
    assert_eq!(field_names(node(&g, "User")), vec!["id"]);
    assert_eq!(field_names(node(&g, "Account")), vec!["email"]);
    assert!(
        g.warnings.iter().any(|w| w.contains("Cannot extend \"User\" as type")),
        "{:?}",
        g.warnings
    );
    assert!(
        g.warnings.iter().any(|w| w.contains("Duplicate field \"Account.email\"")),
        "{:?}",
        g.warnings
    );
}

#[test]
fn extension_warnings_carry_line_and_column() {
    let g = graph("type Query { me: String }\nextend type Ghost { boo: String }\n");
    assert_eq!(g.warnings, vec!["Cannot extend unknown type \"Ghost\" at 2:1"]);
}

// ── root-types.test.ts ─────────────────────────────────────────────────────

#[test]
fn default_root_names_when_no_schema_definition() {
    let g = graph(
        r#"
        type Query { me: User }
        type Mutation { signup: User }
        type User { id: ID! }
        "#,
    );
    assert_eq!(
        g.root_types,
        RootTypeMap {
            query: Some("Query".into()),
            mutation: Some("Mutation".into()),
            subscription: None,
        }
    );
}

#[test]
fn schema_definition_overrides_the_root_operation_names() {
    let g = graph(
        r#"
        schema {
          query: QueryRoot
          mutation: MutationRoot
        }
        type QueryRoot { me: User }
        type MutationRoot { signup: User }
        type User { id: ID! }
        "#,
    );
    assert_eq!(
        g.root_types,
        RootTypeMap {
            query: Some("QueryRoot".into()),
            mutation: Some("MutationRoot".into()),
            subscription: None,
        }
    );
}

#[test]
fn explicit_schema_definition_wins_even_if_a_type_named_query_exists() {
    let g = graph(
        r#"
        schema { query: QueryRoot }
        type QueryRoot { legacy: Query }
        type Query { value: String }
        "#,
    );
    assert_eq!(g.root_types.query.as_deref(), Some("QueryRoot"));
    assert_eq!(g.root_types.mutation, None);
}

#[test]
fn extend_schema_contributes_root_operation_types() {
    let g = graph(
        r#"
        type QueryRoot { me: String }
        type SubRoot { ticks: Int }
        schema { query: QueryRoot }
        extend schema { subscription: SubRoot }
        "#,
    );
    assert_eq!(g.root_types.query.as_deref(), Some("QueryRoot"));
    assert_eq!(g.root_types.subscription.as_deref(), Some("SubRoot"));
}

// ── union-membership.test.ts ───────────────────────────────────────────────

const UNION_SDL: &str = r#"
  type Query { search: [SearchResult!]! feed: [Content!]! }
  type User { id: ID! }
  type Post { id: ID! }
  type Tag { label: String! }
  union SearchResult = User | Post | Tag
  union Content = Post | Tag
"#;

#[test]
fn object_nodes_carry_the_unions_that_include_them_sorted() {
    let g = graph(UNION_SDL);
    assert_eq!(node(&g, "User").member_of_unions.as_ref().unwrap(), &["SearchResult"]);
    assert_eq!(
        node(&g, "Post").member_of_unions.as_ref().unwrap(),
        &["Content", "SearchResult"]
    );
    assert_eq!(
        node(&g, "Tag").member_of_unions.as_ref().unwrap(),
        &["Content", "SearchResult"]
    );
    assert_eq!(node(&g, "Query").member_of_unions, None);
}

#[test]
fn union_nodes_themselves_get_no_member_of_unions() {
    let g = graph(UNION_SDL);
    assert_eq!(node(&g, "SearchResult").member_of_unions, None);
}

// ── overlay.test.ts, the sdl-to-graph half ─────────────────────────────────

#[test]
fn a_plain_schema_treats_a_duplicate_extension_field_as_a_mistake() {
    // Override is the overlay's rule, not the parser's default.
    let g = graph(
        r#"
        type Query { me: User }
        type User { name: String }
        extend type User { name: String! }
        "#,
    );
    assert_eq!(field_types(node(&g, "User")), vec![("name", "String")]);
    assert!(
        g.warnings.iter().any(|w| w.contains("Duplicate field \"User.name\"")),
        "{:?}",
        g.warnings
    );
}

#[test]
fn restating_a_field_overrides_it_in_place() {
    let g = sdl_to_graph(
        r#"
        type Query { user(id: ID!): User }
        type User { id: ID!, name: String }
        extend type User { name: String! }
        "#,
        &SdlToGraphOptions { override_duplicates: true, ..Default::default() },
    );
    // Same position, new type — the card doesn't reshuffle.
    assert_eq!(field_types(node(&g, "User")), vec![("id", "ID!"), ("name", "String!")]);
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
}

#[test]
fn overriding_a_fields_type_repoints_its_edges() {
    let g = sdl_to_graph(
        r#"
        type Query { user(id: ID!): User }
        type User { id: ID! }
        type Report { id: ID! }
        extend type Query { user(id: ID!): Report }
        "#,
        &SdlToGraphOptions { override_duplicates: true, ..Default::default() },
    );
    let ids = edge_ids(&g);
    assert!(ids.contains(&"Query.user->Report"), "{ids:?}");
    assert!(!ids.contains(&"Query.user->User"), "{ids:?}");
}

#[test]
fn overriding_an_enum_values_deprecation_is_applied_in_place() {
    let g = sdl_to_graph(
        r#"
        enum Status { ACTIVE, ARCHIVED }
        extend enum Status {
          ACTIVE @deprecated(reason: "[until 2031-01-01] gone")
        }
        "#,
        &SdlToGraphOptions { override_duplicates: true, ..Default::default() },
    );
    let status = node(&g, "Status");
    assert_eq!(value_names(status), vec!["ACTIVE", "ARCHIVED"]);
    let active = status.values.as_ref().unwrap().iter().find(|v| v.name == "ACTIVE").unwrap();
    assert_eq!(active.until.as_deref(), Some("2031-01-01"));
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
}

#[test]
fn a_duplicate_enum_value_without_override_warns() {
    let g = graph("enum Status { ACTIVE }\nextend enum Status { ACTIVE }\n");
    assert_eq!(value_names(node(&g, "Status")), vec!["ACTIVE"]);
    assert_eq!(g.warnings, vec!["Duplicate value \"Status.ACTIVE\" in extension at 2:1"]);
}

// ── removals ───────────────────────────────────────────────────────────────

fn with_removals(sdl: &str, removals: Vec<SchemaRemoval>) -> ParsedGraph {
    sdl_to_graph(sdl, &SdlToGraphOptions { remove: removals, ..Default::default() })
}

const REMOVAL_SDL: &str = r#"
  type Query { user(id: ID!): User }
  type User { id: ID!, name: String }
  enum Status { ACTIVE, ARCHIVED }
  union Thing = User
"#;

#[test]
fn removing_a_field_drops_it_and_the_edges_it_carried() {
    let g = with_removals(REMOVAL_SDL, vec![SchemaRemoval::member("Query", "user")]);
    assert_eq!(field_names(node(&g, "Query")), Vec::<&str>::new());
    assert!(!edge_ids(&g).contains(&"Query.user->User"));
}

#[test]
fn a_bare_type_removal_drops_the_whole_type() {
    let g = with_removals(REMOVAL_SDL, vec![SchemaRemoval::type_only("Status")]);
    assert!(!g.nodes.iter().any(|n| n.name == "Status"));
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
}

#[test]
fn enum_values_and_union_members_are_removable_too() {
    let g = with_removals(
        REMOVAL_SDL,
        vec![SchemaRemoval::member("Status", "ACTIVE"), SchemaRemoval::member("Thing", "User")],
    );
    assert_eq!(value_names(node(&g, "Status")), vec!["ARCHIVED"]);
    assert_eq!(node(&g, "Thing").members.as_ref().unwrap(), &Vec::<String>::new());
}

#[test]
fn removing_an_implemented_interface_falls_back_to_the_implements_list() {
    let g = with_removals(
        r#"
        interface Timestamped { createdAt: String }
        type Post implements Timestamped { createdAt: String }
        "#,
        vec![SchemaRemoval::member("Post", "Timestamped")],
    );
    assert_eq!(node(&g, "Post").interfaces.as_ref().unwrap(), &Vec::<String>::new());
    assert!(!edge_ids(&g).contains(&"Timestamped-impl-Post"));
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
}

#[test]
fn a_removal_that_matches_nothing_warns_and_changes_nothing_else() {
    let g = with_removals(REMOVAL_SDL, vec![SchemaRemoval::member("User", "nope")]);
    assert_eq!(field_names(node(&g, "User")), vec!["id", "name"]);
    assert_eq!(g.warnings, vec!["Cannot remove \"User.nope\" — no such field"]);
}

#[test]
fn removals_against_a_missing_type_warn_with_the_right_wording() {
    let g = with_removals(
        REMOVAL_SDL,
        vec![SchemaRemoval::type_only("Ghost"), SchemaRemoval::member("Ghost", "boo")],
    );
    assert_eq!(
        g.warnings,
        vec![
            "Cannot remove unknown type \"Ghost\"",
            "Cannot remove \"Ghost.boo\" — no type \"Ghost\"",
        ]
    );
}

#[test]
fn removals_run_after_extensions() {
    // `-report` takes away a field the extension in the same document added.
    let g = with_removals(
        r#"
        type Query { me: String }
        extend type Query { report: Report }
        type Report { id: ID! }
        "#,
        vec![SchemaRemoval::member("Query", "report")],
    );
    assert_eq!(field_names(node(&g, "Query")), vec!["me"]);
    assert!(!edge_ids(&g).contains(&"Query.report->Report"));
}

// ── duplicate / conflicting declarations ───────────────────────────────────

#[test]
fn duplicate_declarations_of_the_same_kind_are_reported_with_positions() {
    let g = graph("type User { id: ID! }\ntype User { name: String }\n");
    assert_eq!(g.warnings, vec!["Duplicate type \"User\" at 1:1, 2:1"]);
}

#[test]
fn conflicting_declarations_name_each_kind() {
    let g = graph("type Thing { id: ID! }\nenum Thing { A }\n");
    assert_eq!(
        g.warnings,
        vec!["Conflicting declarations for \"Thing\": type at 1:1, enum at 2:1"]
    );
}

#[test]
fn extensions_are_not_counted_as_duplicate_declarations() {
    let g = graph("type User { id: ID! }\nextend type User { name: String }\n");
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
}

#[test]
fn every_kind_is_named_correctly_in_a_duplicate_report() {
    let g = graph("scalar A\nscalar A\ninput B { x: Int }\nunion B = A\n");
    assert_eq!(
        g.warnings,
        vec![
            "Duplicate scalar \"A\" at 1:1, 2:1",
            "Conflicting declarations for \"B\": input at 3:1, union at 4:1",
        ]
    );
}

#[test]
fn a_description_block_does_not_shift_the_reported_position() {
    // graphql-js reports a definition's location from the start of its
    // description, and so does the Rust parser — the two must agree.
    let g = graph("\"\"\"doc\"\"\"\ntype User { id: ID! }\n\ntype User { name: String }\n");
    assert_eq!(g.warnings, vec!["Duplicate type \"User\" at 1:1, 4:1"]);
}

// ── documented divergences from graphql-js ─────────────────────────────────

#[test]
fn schema_level_validation_the_rust_parser_performs_but_graphql_js_does_not() {
    // See the module docs: these two are rejected at parse time here, where
    // the TS would have handed back a graph.
    let g = graph("schema { mutation: M }\ntype M { a: String }\n");
    assert_eq!(g.error.as_deref(), Some("schema definition is missing query root"));

    let g = graph("type Q { a: String }\nschema { query: Q query: Q }\n");
    assert_eq!(g.error.as_deref(), Some("multiple query roots in schema definition"));

    // Split across two blocks it parses, and the last one written wins.
    let g = graph("type A { x: Int }\ntype B { x: Int }\nschema { query: A }\nextend schema { query: B }\n");
    assert_eq!(g.root_types.query.as_deref(), Some("B"));
}

// ── Relay unwrapping ───────────────────────────────────────────────────────

const RELAY_SDL: &str = r#"
  type Query { users(first: Int): UserConnection }
  type UserConnection { edges: [UserEdge!]! pageInfo: PageInfo! }
  type UserEdge { node: User! cursor: String! }
  type PageInfo { hasNextPage: Boolean! hasPreviousPage: Boolean! }
  interface Node { id: ID! }
  type User implements Node { id: ID! }
"#;

#[test]
fn relay_boilerplate_is_folded_away_by_default() {
    let g = graph(RELAY_SDL);
    let names: HashSet<&str> = g.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, ["Query", "User"].into_iter().collect::<HashSet<_>>());

    let users = field(node(&g, "Query"), "users");
    // The displayed type is left intact so the panel still reads true.
    assert_eq!(users.type_, "UserConnection");
    assert_eq!(users.type_name, "User");
    assert!(users.is_relay_connection);

    assert_eq!(edge_ids(&g), vec!["Query.users->User"]);
}

#[test]
fn relay_types_survive_when_the_boilerplate_is_shown() {
    let g = sdl_to_graph(
        RELAY_SDL,
        &SdlToGraphOptions { hide_relay_boilerplate: false, ..Default::default() },
    );
    assert_eq!(g.nodes.len(), 6);
    let users = field(node(&g, "Query"), "users");
    assert_eq!(users.type_name, "UserConnection");
    assert!(!users.is_relay_connection);
    let ids = edge_ids(&g);
    assert!(ids.contains(&"Query.users->UserConnection"), "{ids:?}");
    assert!(ids.contains(&"Node-impl-User"), "{ids:?}");
}

#[test]
fn a_lookalike_edge_type_is_left_alone() {
    // Name pattern alone isn't enough — `GraphEdge` lacks node+cursor.
    let g = graph(
        r#"
        type Query { edge: GraphEdge }
        type GraphEdge { weight: Int }
        "#,
    );
    assert!(g.nodes.iter().any(|n| n.name == "GraphEdge"));
    assert_eq!(edge_ids(&g), vec!["Query.edge->GraphEdge"]);
}

// ── edge building ──────────────────────────────────────────────────────────

#[test]
fn input_args_build_a_chain_and_enum_args_get_a_direct_edge() {
    let g = graph(
        r#"
        type Query { search(filter: SearchFilter, sort: SortOrder): Result }
        input SearchFilter { term: String }
        enum SortOrder { ASC DESC }
        type Result { id: ID! }
        "#,
    );
    assert_eq!(
        edge_ids(&g),
        vec![
            "Query.search:enumArg->SortOrder",
            "Query.search->SearchFilter",
            "Query.search:chain1->Result",
        ]
    );

    let head = &g.edges[1];
    assert_eq!(head.kind, EdgeKind::Field);
    assert_eq!(head.source, "Query");
    assert_eq!(head.target, "SearchFilter");
    assert_eq!(head.source_field.as_deref(), Some("search"));
    assert_eq!(head.source_field_index, Some(0));
    assert_eq!(head.label.as_deref(), Some("search"));
    assert_eq!(head.nullable, Some(true));

    let tail = &g.edges[2];
    assert_eq!(tail.kind, EdgeKind::Arg);
    assert_eq!(tail.source, "SearchFilter");
    assert_eq!(tail.source_field, None);
    assert_eq!(tail.nullable, Some(false));

    assert_eq!(g.edges[0].kind, EdgeKind::Arg);
}

#[test]
fn a_scalar_return_is_omitted_from_the_arg_chain() {
    let g = graph(
        r#"
        type Mutation { rename(input: RenameInput!): String }
        input RenameInput { name: String! }
        "#,
    );
    assert_eq!(edge_ids(&g), vec!["Mutation.rename->RenameInput"]);
    assert_eq!(g.edges[0].kind, EdgeKind::Field);
    assert_eq!(g.edges[0].nullable, Some(true));
}

#[test]
fn a_plain_scalar_field_produces_no_edge() {
    let g = graph("type Query { count(after: String): Int }");
    assert!(g.edges.is_empty(), "{:?}", edge_ids(&g));
}

#[test]
fn self_referential_fields_produce_no_edge() {
    let g = graph(
        r#"
        type Query { me: User }
        type User { id: ID! parent: User }
        "#,
    );
    assert_eq!(edge_ids(&g), vec!["Query.me->User"]);
}

#[test]
fn repeated_enum_args_are_deduped_but_repeated_input_args_are_not() {
    let g = graph(
        r#"
        type Query { pick(a: Sort, b: Sort, x: Filter, y: Filter): Result }
        enum Sort { ASC }
        input Filter { term: String }
        type Result { id: ID! }
        "#,
    );
    assert_eq!(
        edge_ids(&g),
        vec![
            "Query.pick:enumArg->Sort",
            "Query.pick->Filter",
            "Query.pick:chain1->Filter",
            "Query.pick:chain2->Result",
        ]
    );
}

#[test]
fn implements_edges_run_interface_to_concrete_type() {
    let g = graph(
        r#"
        type Query { post: Post }
        interface Timestamped { createdAt: String }
        type Post implements Timestamped { createdAt: String }
        "#,
    );
    let e = g.edges.iter().find(|e| e.kind == EdgeKind::Implements).unwrap();
    assert_eq!((e.source.as_str(), e.target.as_str()), ("Timestamped", "Post"));
    assert_eq!(e.label.as_deref(), Some("implements"));
    assert_eq!(e.nullable, None);
}

#[test]
fn union_members_get_labelled_edges() {
    let g = graph(
        r#"
        type Query { thing: Thing }
        union Thing = Post
        type Post { id: ID! }
        "#,
    );
    let e = g.edges.iter().find(|e| e.kind == EdgeKind::Union).unwrap();
    assert_eq!(e.id, "Thing-union-Post");
    assert_eq!(e.label.as_deref(), Some("member"));
}

#[test]
fn edges_pointing_at_dropped_nodes_are_filtered_out() {
    let g = graph("type Query { ghost: Ghost }");
    assert!(g.nodes.iter().all(|n| n.name != "Ghost"));
    assert!(g.edges.is_empty());
}

// ── node building basics ───────────────────────────────────────────────────

#[test]
fn descriptions_kinds_and_nullability_are_captured() {
    let g = graph(
        r#"
        "The entry point."
        type Query { me: User }
        """A person."""
        type User { id: ID! nicknames: [String!] }
        scalar Email
        input Draft { body: String }
        "#,
    );
    assert_eq!(node(&g, "Query").description.as_deref(), Some("The entry point."));
    assert_eq!(node(&g, "User").description.as_deref(), Some("A person."));
    assert_eq!(node(&g, "Email").kind, NodeKind::Scalar);
    assert_eq!(node(&g, "Email").fields, None);
    assert_eq!(node(&g, "Draft").kind, NodeKind::Input);

    let user = node(&g, "User");
    assert!(!field(user, "id").nullable);
    assert!(field(user, "nicknames").nullable);
    assert_eq!(field(user, "nicknames").type_, "[String!]");
    assert_eq!(field(user, "nicknames").type_name, "String");
    // Input-object fields carry no argument list at all.
    assert_eq!(field(node(&g, "Draft"), "body").args, None);
    assert_eq!(field(user, "id").args.as_deref(), Some(&[][..]));
}

#[test]
fn an_empty_document_yields_an_empty_graph() {
    let g = graph("   \n  ");
    assert_eq!(g, ParsedGraph::default());
    assert_eq!(g.error, None);
}

#[test]
fn a_syntax_error_comes_back_as_an_error_not_a_panic() {
    let g = graph("type Query {");
    assert!(g.error.is_some());
    assert!(g.nodes.is_empty());
    assert!(g.edges.is_empty());
    assert_eq!(g.root_types, RootTypeMap::default());
}

// ── large-schema integration ───────────────────────────────────────────────

/// Parses the real production schema, both to prove the port survives a
/// 72k-line document and to keep an eye on parse cost.
#[test]
fn parses_the_production_schema() {
    const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../schema.docs.graphql");
    let sdl = std::fs::read_to_string(PATH).expect("schema.docs.graphql");

    let started = std::time::Instant::now();
    let g = sdl_to_graph(&sdl, &SdlToGraphOptions::default());
    let elapsed = started.elapsed();

    assert_eq!(g.error, None, "schema failed to parse");
    assert!(g.nodes.len() > 1000, "only {} nodes", g.nodes.len());

    let components = weakly_connected_components(&g.nodes, &g.edges);
    let reachable = all_reachable_ids(&g.nodes, &g.edges, &default_root_ops());

    println!(
        "schema.docs.graphql: {} lines, {} nodes, {} edges, {} warnings, \
         {} components, {} reachable — parsed in {:.1?}",
        sdl.lines().count(),
        g.nodes.len(),
        g.edges.len(),
        g.warnings.len(),
        components.len(),
        reachable.len(),
        elapsed,
    );

    // Every component partitions the node set exactly once.
    let total: usize = components.iter().map(|c| c.node_ids.len()).sum();
    assert_eq!(total, g.nodes.len());
    // Root operations resolved, and the graph is actually wired up.
    assert!(g.root_types.query.is_some());
    assert!(g.edges.len() > 1000);
}
