import { describe, expect, test } from "bun:test";
import { filterFields } from "./field-filter";
import { sdlToGraph } from "./sdl-to-graph";

const SDL = `
  interface Entity { id: ID! owner: Account }
  interface Named implements Entity { id: ID! owner: Account name: String }
  type User implements Named & Entity { id: ID! owner: Account name: String posts: [Post] }
  type Account { id: ID! }
  type Post { id: ID! }
  type Query { me: User }
`;

const graph = sdlToGraph(SDL, { hideRelayBoilerplate: false });
const node = (nodes: typeof graph.nodes, id: string) => nodes.find((n) => n.id === id)!;
const fieldNames = (nodes: typeof graph.nodes, id: string) =>
  node(nodes, id).fields!.map((f) => f.name);

describe("filterFields", () => {
  test("off leaves the graph untouched", () => {
    const out = filterFields(graph.nodes, graph.edges, graph.nodes, {
      hidePrimitiveFields: false,
      hideInterfaceFields: false,
    });
    expect(out.nodes).toBe(graph.nodes);
    expect(out.edges).toBe(graph.edges);
  });

  test("hides fields inherited from interfaces, transitively", () => {
    const out = filterFields(graph.nodes, graph.edges, graph.nodes, {
      hidePrimitiveFields: false,
      hideInterfaceFields: true,
    });
    expect(fieldNames(out.nodes, "User")).toEqual(["posts"]);
    expect(fieldNames(out.nodes, "Named")).toEqual(["name"]);
    // The interface that declares them keeps them.
    expect(fieldNames(out.nodes, "Entity")).toEqual(["id", "owner"]);
  });

  test("drops edges from hidden fields and remaps the survivors", () => {
    const out = filterFields(graph.nodes, graph.edges, graph.nodes, {
      hidePrimitiveFields: false,
      hideInterfaceFields: true,
    });
    const fromUser = out.edges.filter((e) => e.source === "User" && e.kind === "field");
    expect(fromUser.map((e) => [e.sourceField, e.sourceFieldIndex])).toEqual([["posts", 0]]);
    // Implements edges are not field edges and survive.
    expect(out.edges.some((e) => e.kind === "implements" && e.target === "User")).toBe(true);
    // The interface's own edge is still drawn.
    expect(out.edges.some((e) => e.source === "Entity" && e.sourceField === "owner")).toBe(true);
  });

  test("resolves interfaces outside the filtered subset", () => {
    const subset = graph.nodes.filter((n) => n.id === "User");
    const out = filterFields(subset, [], graph.nodes, {
      hidePrimitiveFields: false,
      hideInterfaceFields: true,
    });
    expect(fieldNames(out.nodes, "User")).toEqual(["posts"]);
  });

  test("keeps a hidden primitive field's edge to its input argument", () => {
    const g = sdlToGraph(
      `input DeleteInput { id: ID! } type Mutation { del(input: DeleteInput!): Boolean ok: Boolean }`,
      { hideRelayBoilerplate: false },
    );
    const out = filterFields(g.nodes, g.edges, g.nodes, {
      hidePrimitiveFields: true,
      hideInterfaceFields: false,
    });
    const edge = out.edges.find((e) => e.source === "Mutation" && e.target === "DeleteInput");
    expect(edge).toBeDefined();
    expect(edge!.sourceFieldIndex).toBeUndefined();
  });

  test("combines with the primitive filter", () => {
    const out = filterFields(graph.nodes, graph.edges, graph.nodes, {
      hidePrimitiveFields: true,
      hideInterfaceFields: false,
    });
    expect(fieldNames(out.nodes, "User")).toEqual(["owner", "posts"]);
  });
});
