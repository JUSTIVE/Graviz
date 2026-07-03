import { test, expect } from "bun:test";
import { sdlToGraph } from "./sdl-to-graph";
import { allReachableIds, reachableFrom } from "./reachable";

test("default root names when no schema definition", () => {
  const sdl = `
    type Query { me: User }
    type Mutation { signup: User }
    type User { id: ID! }
  `;
  const { rootTypes } = sdlToGraph(sdl);
  expect(rootTypes).toEqual({ query: "Query", mutation: "Mutation", subscription: null });
});

test("schema { query: QueryRoot } overrides the root operation names", () => {
  const sdl = `
    schema {
      query: QueryRoot
      mutation: MutationRoot
    }
    type QueryRoot { me: User }
    type MutationRoot { signup: User }
    type User { id: ID! }
  `;
  const { rootTypes } = sdlToGraph(sdl);
  expect(rootTypes).toEqual({
    query: "QueryRoot",
    mutation: "MutationRoot",
    subscription: null,
  });
});

test("explicit schema definition wins even if a type named Query exists", () => {
  // A renamed-root schema may still contain an ordinary type literally
  // named `Query`; it must NOT be treated as a root.
  const sdl = `
    schema { query: QueryRoot }
    type QueryRoot { legacy: Query }
    type Query { value: String }
  `;
  const { rootTypes } = sdlToGraph(sdl);
  expect(rootTypes.query).toBe("QueryRoot");
});

test("extend schema contributes root operation types", () => {
  const sdl = `
    type QueryRoot { me: String }
    type SubRoot { ticks: Int }
    schema { query: QueryRoot }
    extend schema { subscription: SubRoot }
  `;
  const { rootTypes } = sdlToGraph(sdl);
  expect(rootTypes.query).toBe("QueryRoot");
  expect(rootTypes.subscription).toBe("SubRoot");
});

test("reachability traverses from a renamed query root", () => {
  const sdl = `
    schema { query: QueryRoot }
    type QueryRoot { me: User }
    type User { id: ID! friend: Friend }
    type Friend { id: ID! }
    type Orphan { id: ID! }
  `;
  const graph = sdlToGraph(sdl);
  const rootOps = new Set(["QueryRoot"]);

  const { nodes } = reachableFrom(graph.nodes, graph.edges, "QueryRoot", rootOps);
  const ids = new Set(nodes.map((n) => n.id));
  expect(ids.has("QueryRoot")).toBe(true);
  expect(ids.has("User")).toBe(true);
  expect(ids.has("Friend")).toBe(true);
  expect(ids.has("Orphan")).toBe(false);

  // Orphan detection should key off the renamed root, not the literal "Query".
  const reachable = allReachableIds(graph.nodes, graph.edges, rootOps);
  expect(reachable.has("Orphan")).toBe(false);
  expect(reachable.has("User")).toBe(true);
});
