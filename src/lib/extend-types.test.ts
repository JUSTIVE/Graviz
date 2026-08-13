import { test, expect } from "bun:test";
import { sdlToGraph } from "./sdl-to-graph";

test("extend type appends fields to the existing node", () => {
  const { nodes, warnings } = sdlToGraph(`
    type Query { user(id: ID!): User }
    type User { id: ID! }
    extend type Query { report: Report }
    type Report { id: ID!, owner: User! }
  `);
  const query = nodes.find((n) => n.name === "Query")!;
  expect(query.fields?.map((f) => f.name)).toEqual(["user", "report"]);
  expect(warnings).toEqual([]);
});

test("fields added by an extension produce graph edges", () => {
  const { edges } = sdlToGraph(`
    type Query { user(id: ID!): User }
    type User { id: ID! }
    extend type Query { report: Report }
    type Report { owner: User! }
  `);
  const ids = edges.map((e) => e.id);
  expect(ids).toContain("Query.report->Report");
  expect(ids).toContain("Report.owner->User");
});

test("extension fields keep their deprecation metadata", () => {
  const { nodes } = sdlToGraph(`
    type Query { me: User }
    type User { id: ID! }
    extend type User {
      legacyName: String @deprecated(reason: "[until 2030-01-01] use fullName")
    }
  `);
  const field = nodes.find((n) => n.name === "User")!.fields!.find((f) => f.name === "legacyName")!;
  expect(field.isDeprecated).toBe(true);
  expect(field.until).toBe("2030-01-01");
});

test("extend enum / union / interface merge into their targets", () => {
  const { nodes } = sdlToGraph(`
    type Query { thing: Thing }
    enum Status { ACTIVE }
    extend enum Status { ARCHIVED }
    union Thing = Post
    type Post implements Timestamped { id: ID! }
    extend union Thing = Comment
    type Comment { id: ID! }
    interface Timestamped { createdAt: String }
    extend interface Timestamped { updatedAt: String }
  `);
  expect(nodes.find((n) => n.name === "Status")!.values?.map((v) => v.name)).toEqual([
    "ACTIVE",
    "ARCHIVED",
  ]);
  expect(nodes.find((n) => n.name === "Thing")!.members).toEqual(["Post", "Comment"]);
  expect(
    nodes.find((n) => n.name === "Timestamped")!.fields?.map((f) => f.name),
  ).toEqual(["createdAt", "updatedAt"]);
});

test("extend type can add an interface implementation", () => {
  const { nodes, edges } = sdlToGraph(`
    type Query { post: Post }
    interface Timestamped { createdAt: String }
    type Post { id: ID! }
    extend type Post implements Timestamped { createdAt: String }
  `);
  expect(nodes.find((n) => n.name === "Post")!.interfaces).toEqual(["Timestamped"]);
  expect(edges.map((e) => e.id)).toContain("Timestamped-impl-Post");
});

test("extending an unknown type warns instead of throwing", () => {
  const { nodes, warnings, error } = sdlToGraph(`
    type Query { me: User }
    type User { id: ID! }
    extend type Ghost { boo: String }
  `);
  expect(error).toBeNull();
  expect(nodes.some((n) => n.name === "Ghost")).toBe(false);
  expect(warnings.some((w) => w.includes('Cannot extend unknown type "Ghost"'))).toBe(true);
});

test("kind mismatch and duplicate members are warned and skipped", () => {
  const { nodes, warnings } = sdlToGraph(`
    type Query { me: User }
    interface User { id: ID! }
    extend type User { name: String }
    type Account { email: String }
    extend type Account { email: String }
  `);
  expect(nodes.find((n) => n.name === "User")!.fields?.map((f) => f.name)).toEqual(["id"]);
  expect(nodes.find((n) => n.name === "Account")!.fields?.map((f) => f.name)).toEqual(["email"]);
  expect(warnings.some((w) => w.includes('Cannot extend "User" as type'))).toBe(true);
  expect(warnings.some((w) => w.includes('Duplicate field "Account.email"'))).toBe(true);
});
