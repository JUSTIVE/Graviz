import { test, expect } from "bun:test";
import { declaredTypeNames, markOverlay, mergeSdl, prepareOverlay } from "./overlay";
import { sdlToGraph } from "./sdl-to-graph";

const BASE = `
  type Query { user(id: ID!): User }
  type User { id: ID!, name: String }
  enum Status { ACTIVE }
`;

/** Parses base and base+overlay the way the schema context does. */
function apply(overlay: string) {
  const base = sdlToGraph(BASE);
  const prepared = prepareOverlay(BASE, overlay);
  const merged = sdlToGraph(prepared.sdl, {
    remove: prepared.removals,
    overrideDuplicates: true,
  });
  expect(merged.error).toBeNull();
  return {
    ...markOverlay(base, merged),
    warnings: [
      ...prepared.warnings,
      ...merged.warnings.filter((w) => !base.warnings.includes(w)),
    ],
  };
}

test("mergeSdl leaves the base untouched when the overlay is blank", () => {
  expect(mergeSdl("type Query { a: String }", "   \n ")).toBe("type Query { a: String }");
});

test("a brand-new type is flagged on the node", () => {
  const { graph, diff } = apply(`
    extend type Query { report: Report }
    type Report { id: ID! }
  `);
  const report = graph.nodes.find((n) => n.name === "Report")!;
  expect(report.isOverlay).toBe(true);
  expect(diff.addedTypes).toEqual(["Report"]);
});

test("a field added to an existing type is flagged on the row, not the node", () => {
  const { graph, diff } = apply(`extend type User { email: String }`);
  const user = graph.nodes.find((n) => n.name === "User")!;
  expect(user.isOverlay).toBeUndefined();
  expect(user.fields!.find((f) => f.name === "email")!.isOverlay).toBe(true);
  expect(user.fields!.find((f) => f.name === "id")!.isOverlay).toBeUndefined();
  expect(diff.addedFields).toEqual(["User.email"]);
  expect(diff.addedTypes).toEqual([]);
});

test("enum values added by an extension are flagged", () => {
  const { graph, diff } = apply(`extend enum Status { ARCHIVED }`);
  const status = graph.nodes.find((n) => n.name === "Status")!;
  expect(status.values!.map((v) => [v.name, v.isOverlay])).toEqual([
    ["ACTIVE", undefined],
    ["ARCHIVED", true],
  ]);
  expect(diff.addedFields).toEqual(["Status.ARCHIVED"]);
});

test("untouched nodes keep their identity so the canvas cache survives", () => {
  const base = sdlToGraph(BASE);
  const merged = sdlToGraph(mergeSdl(BASE, `extend type User { email: String }`));
  const { graph } = markOverlay(base, merged);
  const mergedQuery = merged.nodes.find((n) => n.name === "Query")!;
  const markedQuery = graph.nodes.find((n) => n.name === "Query")!;
  // Query gained nothing — markOverlay must hand back the very same object.
  expect(markedQuery).toBe(mergedQuery);
});

test("an overlay restating a field verbatim reports an empty diff", () => {
  const { diff } = apply(`extend type User { name: String }`);
  expect(diff).toEqual({
    addedTypes: [],
    addedFields: [],
    changedFields: [],
    removedTypes: [],
    removedFields: [],
  });
});

// ── Overriding: restating a field replaces it ──

test("restating a field with a different type overrides it in place", () => {
  const { graph, diff, warnings } = apply(`
    type User {
      name: String!
    }
  `);
  const user = graph.nodes.find((n) => n.name === "User")!;
  // Same position, new type — the card doesn't reshuffle.
  expect(user.fields!.map((f) => [f.name, f.type])).toEqual([
    ["id", "ID!"],
    ["name", "String!"],
  ]);
  expect(diff.changedFields).toEqual(["User.name"]);
  expect(diff.addedFields).toEqual([]);
  expect(warnings).toEqual([]);
});

test("an overridden row is flagged like an added one so the canvas marks it", () => {
  const { graph } = apply(`type User { name: String! }`);
  const user = graph.nodes.find((n) => n.name === "User")!;
  expect(user.fields!.find((f) => f.name === "name")!.isOverlay).toBe(true);
  expect(user.fields!.find((f) => f.name === "id")!.isOverlay).toBeUndefined();
});

test("overriding a field's type re-points its edges", () => {
  const { graph } = apply(`
    type Report { id: ID! }
    type Query { user(id: ID!): Report }
  `);
  const ids = graph.edges.map((e) => e.id);
  expect(ids).toContain("Query.user->Report");
  expect(ids).not.toContain("Query.user->User");
});

test("overriding an argument list counts as a change", () => {
  const { diff } = apply(`type Query { user(id: ID!, slug: String): User }`);
  expect(diff.changedFields).toEqual(["Query.user"]);
});

test("overriding an enum value's deprecation counts as a change", () => {
  const { graph, diff } = apply(`
    enum Status {
      ACTIVE @deprecated(reason: "[until 2031-01-01] gone")
    }
  `);
  const status = graph.nodes.find((n) => n.name === "Status")!;
  const active = status.values!.find((v) => v.name === "ACTIVE")!;
  expect(active.until).toBe("2031-01-01");
  expect(active.isOverlay).toBe(true);
  expect(diff.changedFields).toEqual(["Status.ACTIVE"]);
});

test("a plain schema still treats a duplicate extension field as a mistake", () => {
  // Override is the overlay's rule, not the parser's default.
  const { nodes, warnings } = sdlToGraph(`
    type Query { me: User }
    type User { name: String }
    extend type User { name: String! }
  `);
  expect(nodes.find((n) => n.name === "User")!.fields!.map((f) => f.type)).toEqual([
    "String",
  ]);
  expect(warnings.some((w) => w.includes('Duplicate field "User.name"'))).toBe(true);
});

// ── Augmenting: redeclaring a known type merges instead of colliding ──

test("declaredTypeNames lists definitions but not extensions", () => {
  const names = declaredTypeNames(`
    type Query { a: String }
    enum Status { ACTIVE }
    extend type Query { b: String }
    extend type Ghost { c: String }
  `);
  expect([...names].sort()).toEqual(["Query", "Status"]);
});

test("a bare redeclaration of an existing type augments it", () => {
  const { graph, diff, warnings } = apply(`
    type User { email: String }
  `);
  const user = graph.nodes.find((n) => n.name === "User")!;
  expect(user.fields!.map((f) => f.name)).toEqual(["id", "name", "email"]);
  expect(diff.addedFields).toEqual(["User.email"]);
  expect(warnings).toEqual([]);
  // One node, not two — the rewrite is what keeps the ids unique.
  expect(graph.nodes.filter((n) => n.name === "User")).toHaveLength(1);
});

test("augmenting also works for enums, and a type the schema lacks stays new", () => {
  const { graph, diff } = apply(`
    enum Status { ARCHIVED }
    type Report { id: ID! }
  `);
  expect(graph.nodes.find((n) => n.name === "Status")!.values!.map((v) => v.name)).toEqual([
    "ACTIVE",
    "ARCHIVED",
  ]);
  expect(diff.addedTypes).toEqual(["Report"]);
  expect(diff.addedFields).toEqual(["Status.ARCHIVED"]);
});

test("a redeclaration carrying its own description still parses", () => {
  // `extend type User` may not take a description, so the rewrite has to
  // drop the doc block a pasted type brought with it.
  const { graph, diff } = apply(`
    """A person."""
    type User {
      email: String
    }
  `);
  expect(graph.nodes.find((n) => n.name === "User")!.fields!.map((f) => f.name)).toEqual([
    "id",
    "name",
    "email",
  ]);
  expect(diff.addedFields).toEqual(["User.email"]);
});

test("an already-written extension is left alone", () => {
  const prepared = prepareOverlay(BASE, `extend type User { email: String }`);
  expect(prepared.sdl).not.toContain("extend extend");
});

// ── Removing: `-name` takes something away ──

test("`-field` inside a block removes that field", () => {
  const { graph, diff, warnings } = apply(`
    type User {
      -name
      email: String
    }
  `);
  const user = graph.nodes.find((n) => n.name === "User")!;
  expect(user.fields!.map((f) => f.name)).toEqual(["id", "email"]);
  expect(diff.removedFields).toEqual(["User.name"]);
  expect(diff.addedFields).toEqual(["User.email"]);
  expect(warnings).toEqual([]);
});

test("removing a field drops the edges it carried", () => {
  const { graph } = apply(`
    type Query {
      -user
    }
  `);
  expect(graph.nodes.find((n) => n.name === "Query")!.fields).toEqual([]);
  expect(graph.edges.map((e) => e.id)).not.toContain("Query.user->User");
});

test("a block emptied by removals is dropped, description and all", () => {
  // `extend type Query { }` is a syntax error, so the whole block — plus
  // the doc comment that would then dangle — has to come out.
  const { graph, diff, warnings } = apply(`
    """Root."""
    type Query {
      -user
    }
  `);
  expect(graph.nodes.find((n) => n.name === "Query")!.fields).toEqual([]);
  expect(diff.removedFields).toEqual(["Query.user"]);
  expect(warnings).toEqual([]);
});

test("`-Type.field` works from the top level", () => {
  const { diff } = apply(`-User.name`);
  expect(diff.removedFields).toEqual(["User.name"]);
  expect(diff.removedTypes).toEqual([]);
});

test("a bare `-Type` at the top level removes the whole type", () => {
  const { graph, diff } = apply(`-Status`);
  expect(graph.nodes.some((n) => n.name === "Status")).toBe(false);
  expect(diff.removedTypes).toEqual(["Status"]);
  expect(diff.removedFields).toEqual([]);
});

test("enum values are removable too", () => {
  const { graph, diff } = apply(`
    enum Status {
      -ACTIVE
      ARCHIVED
    }
  `);
  expect(graph.nodes.find((n) => n.name === "Status")!.values!.map((v) => v.name)).toEqual([
    "ARCHIVED",
  ]);
  expect(diff.removedFields).toEqual(["Status.ACTIVE"]);
});

test("a removal that matches nothing warns and changes nothing else", () => {
  const { graph, diff, warnings } = apply(`
    type User {
      -nope
      email: String
    }
  `);
  expect(graph.nodes.find((n) => n.name === "User")!.fields!.map((f) => f.name)).toEqual([
    "id",
    "name",
    "email",
  ]);
  expect(diff.removedFields).toEqual([]);
  expect(warnings).toEqual([`Cannot remove "User.nope" — no such field`]);
});

// ── Ordering: the overlay reads top to bottom ──

test("a removal below a definition of the same name wins", () => {
  const { graph, diff } = apply(`
    type User {
      email: String
      -email
    }
  `);
  expect(graph.nodes.find((n) => n.name === "User")!.fields!.map((f) => f.name)).toEqual([
    "id",
    "name",
  ]);
  expect(diff.addedFields).toEqual([]);
  expect(diff.removedFields).toEqual([]);
});

test("a definition below a removal of the same name wins", () => {
  const { graph, diff, warnings } = apply(`
    type User {
      email: Email!
      -email
      email: Email!
    }
    scalar Email
  `);
  const user = graph.nodes.find((n) => n.name === "User")!;
  expect(user.fields!.map((f) => [f.name, f.type])).toEqual([
    ["id", "ID!"],
    ["name", "String"],
    ["email", "Email!"],
  ]);
  expect(diff.addedFields).toEqual(["User.email"]);
  expect(diff.removedFields).toEqual([]);
  expect(warnings).toEqual([]);
});

test("re-declaring a base field below its removal overrides instead", () => {
  const { graph, diff } = apply(`
    type User {
      -name
      name: String!
    }
  `);
  const user = graph.nodes.find((n) => n.name === "User")!;
  expect(user.fields!.map((f) => [f.name, f.type])).toEqual([
    ["id", "ID!"],
    ["name", "String!"],
  ]);
  expect(diff.changedFields).toEqual(["User.name"]);
  expect(diff.removedFields).toEqual([]);
});

test("ordering holds across blocks, and for a `-Type.field` above one", () => {
  const { graph, diff } = apply(`
    -User.name
    extend type User { name: String! }
  `);
  expect(
    graph.nodes.find((n) => n.name === "User")!.fields!.map((f) => f.type),
  ).toEqual(["ID!", "String!"]);
  expect(diff.changedFields).toEqual(["User.name"]);
  expect(diff.removedFields).toEqual([]);
});

test("a type dropped and then declared again is replaced, not deleted", () => {
  const { graph, diff } = apply(`
    -User
    type User {
      name: String!
      handle: String!
    }
  `);
  const user = graph.nodes.find((n) => n.name === "User")!;
  // `id` was the schema's and nothing below asked for it back; `name` was
  // re-declared, so it stays (with the overlay's type).
  expect(user.fields!.map((f) => [f.name, f.type])).toEqual([
    ["name", "String!"],
    ["handle", "String!"],
  ]);
  expect(diff.removedTypes).toEqual([]);
  expect(diff.removedFields).toEqual(["User.id"]);
});

test("a type declared and then dropped is still gone", () => {
  const { graph, diff } = apply(`
    type User { handle: String! }
    -User
  `);
  expect(graph.nodes.some((n) => n.name === "User")).toBe(false);
  expect(diff.removedTypes).toEqual(["User"]);
});

test("an argument sharing a field's name doesn't count as a definition", () => {
  // `id` inside the arg list must not cancel the `-id` above it.
  const { graph, diff } = apply(`
    type Query {
      user(
        id: ID!
      ): User
    }
    type User {
      -id
    }
  `);
  expect(graph.nodes.find((n) => n.name === "User")!.fields!.map((f) => f.name)).toEqual([
    "name",
  ]);
  expect(diff.removedFields).toEqual(["User.id"]);
});

test("a removal line inside a comment or string is not one", () => {
  const { diff, warnings } = apply(`
    type User {
      """
      -name
      """
      email: String
      # -id
    }
  `);
  expect(diff.removedFields).toEqual([]);
  expect(diff.addedFields).toEqual(["User.email"]);
  expect(warnings).toEqual([]);
});

test("blanked lines keep the parse error pointing at the right line", () => {
  const prepared = prepareOverlay(BASE, `-User.name\n\ntype Report { id: ID! }`);
  const overlayLines = prepared.sdl.slice(BASE.length).split("\n");
  // The `-User.name` line is blanked, not deleted, so what follows keeps
  // its position in the document.
  expect(overlayLines.filter((l) => l.includes("type Report"))).toHaveLength(1);
  expect(prepared.sdl.split("\n")).toHaveLength(
    BASE.split("\n").length + 1 + 3,
  );
});
