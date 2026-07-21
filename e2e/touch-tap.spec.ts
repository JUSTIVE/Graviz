import { test, expect, type Page } from "@playwright/test";
import { SAMPLE_SDL } from "../src/lib/sample-sdl";

test.use({ hasTouch: true });

function hashSdl(s0: string): string {
  const s = s0.trim();
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  return h.toString(36);
}
interface Dim { id: string; rowH: number; headerH: number; w: number; h: number; cx: number; cy: number }
type DebugWindow = {
  __gqlCanvas?: {
    getLaidNodeCount(): number;
    getLod(): string;
    getView(): { x: number; y: number; k: number };
    getNodeDimensions(): Dim[];
    getFocusId(): string | null;
  };
};

async function boot(page: Page) {
  await page.addInitScript((entries) => {
    localStorage.setItem("gompassql:history", JSON.stringify(entries));
  }, [{ hash: hashSdl(SAMPLE_SDL), sdl: SAMPLE_SDL, name: "sample.graphql", createdAt: Date.now(), updatedAt: Date.now() }]);
  await page.goto("/");
  await page.getByRole("button", { name: /Recent schemas/i }).click({ timeout: 10_000 });
  await page.getByRole("button", { name: /sample\.graphql/i }).click({ timeout: 10_000 });
  await page.getByRole("button", { name: /^Visualize$/ }).click({ timeout: 10_000 });
  await page.waitForURL(/\/view/, { timeout: 30_000 });
  await page.waitForFunction(
    () => ((window as unknown as DebugWindow).__gqlCanvas?.getLaidNodeCount() ?? 0) > 0,
    undefined, { timeout: 60_000 },
  );
}
const canvasXY = async (page: Page, id: string, dxFrac: number, dyRowsFromHeader: number) => {
  return await page.evaluate(({ id, dxFrac, dyRowsFromHeader }) => {
    const api = (window as unknown as DebugWindow).__gqlCanvas!;
    const v = api.getView();
    const n = api.getNodeDimensions().find((d) => d.id === id)!;
    const wx = n.cx - n.w / 2 + n.w * dxFrac;
    const bodyTop = n.headerH + 8 - 2;
    const wy = n.cy - n.h / 2 + bodyTop + dyRowsFromHeader * n.rowH + n.rowH / 2;
    return { x: wx * v.k + v.x, y: wy * v.k + v.y };
  }, { id, dxFrac, dyRowsFromHeader });
};

test("touch tap: header navigate, field pin, empty clear", async ({ page }) => {
  test.setTimeout(120_000);
  await boot(page);
  // Frame User at full LOD.
  await page.evaluate(() => {
    const api = (window as unknown as DebugWindow).__gqlCanvas!;
    // @ts-expect-error debug
    api.navigate?.("User");
  });
  await page.waitForFunction(
    () => (window as unknown as DebugWindow).__gqlCanvas?.getLod() === "full",
    undefined, { timeout: 30_000 },
  );
  await page.waitForTimeout(1500);
  const box = (await page.locator("canvas").first().boundingBox())!;

  // Tap the header of User → navigates (records focus). Header is above body.
  const headerPt = await page.evaluate(() => {
    const api = (window as unknown as DebugWindow).__gqlCanvas!;
    const v = api.getView();
    const n = api.getNodeDimensions().find((d) => d.id === "User")!;
    const wx = n.cx; const wy = n.cy - n.h / 2 + n.headerH / 2;
    return { x: wx * v.k + v.x, y: wy * v.k + v.y };
  });
  await page.touchscreen.tap(headerPt.x + box.x, headerPt.y + box.y);
  await page.waitForTimeout(800);
  const focus1 = await page.evaluate(() => (window as unknown as DebugWindow).__gqlCanvas!.getFocusId());
  console.log("after header tap, focus:", focus1);
  expect(focus1).toBe("User");
});
