import { test, expect } from "bun:test";
import { sdlToGraph } from "./sdl-to-graph";

const SDL = `
  type Query { search: [SearchResult!]! feed: [Content!]! }
  type User { id: ID! }
  type Post { id: ID! }
  type Tag { label: String! }
  union SearchResult = User | Post | Tag
  union Content = Post | Tag
`;

test("Object nodes carry the unions that include them, sorted", () => {
  const { nodes } = sdlToGraph(SDL);
  const byId = new Map(nodes.map((n) => [n.id, n]));
  expect(byId.get("User")?.memberOfUnions).toEqual(["SearchResult"]);
  expect(byId.get("Post")?.memberOfUnions).toEqual(["Content", "SearchResult"]);
  expect(byId.get("Tag")?.memberOfUnions).toEqual(["Content", "SearchResult"]);
  expect(byId.get("Query")?.memberOfUnions).toBeUndefined();
});

test("union nodes themselves get no memberOfUnions", () => {
  const { nodes } = sdlToGraph(SDL);
  const union = nodes.find((n) => n.id === "SearchResult");
  expect(union?.memberOfUnions).toBeUndefined();
});
