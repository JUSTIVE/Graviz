import { expect, test, type Page } from "@playwright/test";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Regression coverage for the trailing card sections (implements /
 * member-of-union) and compact Union nodes:
 * - Section rows use the tight ROW_H pitch with gap/centering offsets;
 *   clicking a row must navigate to that exact type — this exercises
 *   trailingSectionGeom, which the painter, hit-test, and hover
 *   overlay all share (they used to disagree and hovers landed a row
 *   off).
 * - Union member rows render compactly and navigate on click.
 */

const here = dirname(fileURLToPath(import.meta.url));

const sdl = `
type Query { search: [SearchResult!]! user: User post: Post }
type User implements Node & Actor {
  """The user id."""
  id: ID!
  name: String!
  posts: [Post!]!
}
type Post implements Node { id: ID! title: String! author: User! }
type Tag { label: String! }
interface Node { id: ID! }
interface Actor { name: String! }
union SearchResult = User | Post | Tag
union Content = Post | Tag
`;

function hashSdl(s0: string): string {
  const s = s0.trim();
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  return h.toString(36);
}

interface Dim {
  id: string;
  rowH: number;
  headerH: number;
  w: number;
  h: number;
  cx: number;
  cy: number;
}
type DebugWindow = {
  __gqlCanvas?: {
    getLaidNodeCount(): number;
    getLod(): string;
    getView(): { x: number; y: number; k: number };
    getNodeDimensions(): Dim[];
    getFocusId(): string | null;
    navigate(id: string): void;
  };
};

test("section rows hit-test and navigate at their painted positions", async ({ page }) => {
  test.setTimeout(120_000);
  await page.addInitScript((entries) => {
    localStorage.setItem("gompassql:history", JSON.stringify(entries));
  }, [{ hash: hashSdl(sdl), sdl, name: "union.graphql", createdAt: Date.now(), updatedAt: Date.now() }]);
  await page.goto("/");
  await page.getByRole("button", { name: /Recent schemas/i }).click({ timeout: 10_000 });
  await page.getByRole("button", { name: /union\.graphql/i }).click({ timeout: 10_000 });
  await page.getByRole("button", { name: /^Visualize$/ }).click({ timeout: 10_000 });
  await page.waitForURL(/\/view/, { timeout: 30_000 });
  await page.waitForFunction(
    () => ((window as unknown as DebugWindow).__gqlCanvas?.getLaidNodeCount() ?? 0) > 0,
    undefined,
    { timeout: 60_000 },
  );
  await page.evaluate(() => (window as unknown as DebugWindow).__gqlCanvas!.navigate("User"));
  await page.waitForFunction(
    () => (window as unknown as DebugWindow).__gqlCanvas?.getLod() === "full",
    undefined,
    { timeout: 30_000 },
  );
  await page.waitForTimeout(2000);

  // Screen positions of User's trailing-section rows. Mirrors
  // trailingSectionGeom: 3 fields, 2 ifaces, 1 union, ROW_H = 14,
  // IMPL_SECTION_GAP = 8, TOP_BODY_PAD = 8; leftover space splits
  // evenly between the two touching bands.
  const pos = await page.evaluate(() => {
    const api = (window as unknown as DebugWindow).__gqlCanvas!;
    const v = api.getView();
    const n = api.getNodeDimensions().find((d) => d.id === "User")!;
    const ROW_H = 14;
    const washTop = n.headerH + 8 + 3 * n.rowH - 2 + 8; // implGap=8
    const ifaceBlockH = 2 * ROW_H;
    const unionBlockH = 1 * ROW_H;
    const extra = Math.max(0, n.h - washTop - ifaceBlockH - unionBlockH);
    const ifaceBandBottom = washTop + ifaceBlockH + Math.floor(extra / 2);
    const ifaceRowsTop =
      washTop +
      Math.max(0, Math.floor((ifaceBandBottom - washTop - ifaceBlockH) / 2));
    const unionRowsTop =
      ifaceBandBottom +
      Math.max(0, Math.floor((n.h - ifaceBandBottom - unionBlockH) / 2));
    const nodeLeft = n.cx - n.w / 2;
    const nodeTop = n.cy - n.h / 2;
    const toScreen = (wx: number, wy: number) => ({
      x: wx * v.k + v.x,
      y: wy * v.k + v.y,
    });
    // Interface row 1 ("Actor") — row 0 is the Relay `Node` interface,
    // folded away by default and therefore intentionally not
    // navigable.
    return {
      iface1: toScreen(nodeLeft + 40, nodeTop + ifaceRowsTop + ROW_H * 1.5),
      union0: toScreen(nodeLeft + 40, nodeTop + unionRowsTop + ROW_H / 2),
    };
  });

  // getView() coordinates are canvas-relative; page.mouse works in
  // viewport coordinates — offset by the canvas element's origin.
  const canvasBox = (await page.locator("canvas").first().boundingBox())!;

  await page.mouse.click(pos.iface1.x + canvasBox.x, pos.iface1.y + canvasBox.y);
  await page.waitForTimeout(800);
  const focusAfterIface = await page.evaluate(
    () => (window as unknown as DebugWindow).__gqlCanvas!.getFocusId(),
  );
  expect(focusAfterIface).toBe("Actor");

  // Back to User, then the union-membership row.
  await page.evaluate(() => (window as unknown as DebugWindow).__gqlCanvas!.navigate("User"));
  await page.waitForTimeout(800);
  await page.mouse.click(pos.union0.x + canvasBox.x, pos.union0.y + canvasBox.y);
  await page.waitForTimeout(800);
  const focusAfterUnion = await page.evaluate(
    () => (window as unknown as DebugWindow).__gqlCanvas!.getFocusId(),
  );
  expect(focusAfterUnion).toBe("SearchResult");

  // Compact Union node visual — member rows at tight pitch, Object
  // color, no "|" prefix.
  await page.waitForTimeout(1500);
  await page.screenshot({
    path: join(here, "__screenshots__", "union-node-compact.png"),
  });
});
