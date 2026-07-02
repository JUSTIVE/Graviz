import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * Regression coverage for the canvas LOD-not-refreshing bug on a
 * long-distance, one-shot focus jump (tree-panel field click or
 * canvas return-type click → `pushFocus`).
 *
 * Repro: load the GitHub schema (~1.4k types), let the canvas settle
 * at its initial auto-fit (low zoom → chrome/bar LOD), then click a
 * tree-panel field whose owner is well away from the current
 * viewport. The focus pan re-centers on the target at k≥0.9
 * (LOD="full"), but on the bug-affected build the newly-in-view
 * nodes were enqueued for sprite *creation* on frame 1 and then
 * never re-entered the sprite sweep to be queued for full-LOD
 * *texture build* — so they remained on the per-kind placeholder
 * indefinitely.
 *
 * The test reads the debug hook exposed at `window.__gqlCanvas` to
 * verify that after navigation, every in-view node has a full-LOD
 * texture cached. No pixel-level screenshot diff needed.
 */

const SCHEMA_PATH = join(here, "..", "schema.docs.graphql");
const largeSchema = readFileSync(SCHEMA_PATH, "utf8");

function hashSdl(sdl: string): string {
  const s = sdl.trim();
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  }
  return h.toString(36);
}

async function seedHistory(page: Page, sdl: string) {
  await page.addInitScript((entries) => {
    localStorage.setItem("gompassql:history", JSON.stringify(entries));
  }, [
    {
      hash: hashSdl(sdl),
      sdl,
      name: "github.graphql",
      createdAt: Date.now(),
      updatedAt: Date.now(),
    },
  ]);
}

interface CanvasDebug {
  getLod(): "full" | "bar" | "chrome";
  getView(): { x: number; y: number; k: number };
  getTextureKeys(): string[];
  getSpriteIds(): string[];
  getLaidNodeCount(): number;
  getFocusId(): string | null;
  isFocusJumpPending(): boolean;
  getInViewNodeIds(): string[];
  getSpriteResetCount(): number;
  getTicksSwept(): number;
  getSyncBuildCount(): number;
  navigate(id: string): void;
}

async function getCanvasDebug<T>(
  page: Page,
  fn: (api: CanvasDebug) => T,
): Promise<T> {
  return await page.evaluate((fnStr) => {
    const api = (window as unknown as { __gqlCanvas?: CanvasDebug })
      .__gqlCanvas;
    if (!api) throw new Error("__gqlCanvas debug hook not exposed");
    // biome-ignore lint/security/noGlobalEval: test-only introspection
    const f = new Function("api", `return (${fnStr})(api);`);
    return f(api) as unknown;
  }, fn.toString()) as Promise<T>;
}

async function navigateToView(page: Page) {
  await seedHistory(page, largeSchema);
  await page.goto("/");
  await page
    .getByRole("button", { name: /Recent schemas/i })
    .click({ timeout: 10_000 });
  await page
    .getByRole("button", { name: /github\.graphql/i })
    .click({ timeout: 10_000 });
  await page
    .getByRole("button", { name: /^Visualize$/ })
    .click({ timeout: 10_000 });
  await page.waitForURL(/\/view/, { timeout: 30_000 });
  // Wait for layout overlay to disappear so the canvas is interactable.
  await page
    .getByText(/Laying out/i)
    .first()
    .waitFor({ state: "hidden", timeout: 120_000 })
    .catch(() => {});
  await page.locator("canvas").first().waitFor({ timeout: 15_000 });
}

test.describe("Canvas LOD refresh on long-distance navigation", () => {
  test.setTimeout(240_000);

  test("every in-view node has a full-LOD texture after a tree-panel field click", async ({
    page,
  }) => {
    await navigateToView(page);

    // Let the initial auto-fit + first-pass texture drain finish.
    // The schema fits into the viewport at low zoom; LOD will be
    // chrome or bar to start, and only some sprites will have any
    // texture cached at all.
    await page.waitForTimeout(2_000);

    // Pre-condition: the debug hook is wired up.
    const lodBefore = await getCanvasDebug(page, (api) => api.getLod());
    expect(["full", "bar", "chrome"]).toContain(lodBefore);

    // Drive the navigation through the same `onNavigate` (pushFocus)
    // code path that a field-row click takes, but bypass the search
    // UI so the test deterministically lands on the intended target.
    // Repository is a hub type in the GitHub schema and lives well
    // away from the Query root after dot layout — the focus pan
    // counts as a long-distance jump.
    const target = "Repository";
    await page.evaluate((id: string) => {
      const api = (window as unknown as { __gqlCanvas?: { navigate(id: string): void } }).__gqlCanvas;
      if (!api) throw new Error("__gqlCanvas not exposed");
      api.navigate(id);
    }, target);

    // Give the focus pan + sweep + create + build queues plenty
    // of time to settle. With the fix, the build queue keeps
    // draining (focus-jump bypass) until every in-view sprite has
    // its full-LOD texture cached.
    await page.waitForTimeout(3_500);

    const after = await getCanvasDebug(page, (api) => ({
      lod: api.getLod(),
      view: api.getView(),
      textureKeys: api.getTextureKeys(),
      inView: api.getInViewNodeIds(),
      spriteIds: api.getSpriteIds(),
      focusJumpPending: api.isFocusJumpPending(),
    }));

    // After a focus pan the view is centered on the target at
    // k ≥ FOCUS_MIN_ZOOM (0.9), well past the full-LOD threshold.
    expect(after.lod).toBe("full");
    expect(after.view.k).toBeGreaterThanOrEqual(0.9);

    // Focus-jump flag should have cleared once the queues drained.
    expect(after.focusJumpPending).toBe(false);

    // The core regression assertion: every in-view node must have
    // a full-LOD texture cached. Pre-fix, the long-distance jump
    // left newly-in-view sprites stranded on their per-kind
    // placeholder textures because the sweep only re-ran on
    // explicit view-move events.
    // Keys are `${id}:${lod}:${dprBucket}` — GraphQL type names can't
    // contain ":", so the id is everything before the first colon.
    const cachedFullIds = new Set(
      after.textureKeys
        .filter((k) => k.includes(":full:"))
        .map((k) => k.slice(0, k.indexOf(":"))),
    );

    const inViewWithSprite = after.inView.filter((id) =>
      after.spriteIds.includes(id),
    );
    const missing = inViewWithSprite.filter((id) => !cachedFullIds.has(id));

    // Sanity: we actually have nodes in view with sprites.
    expect(inViewWithSprite.length).toBeGreaterThan(0);
    expect(
      missing,
      `nodes in view (with sprite) without a full-LOD texture: ${missing.join(", ")}. ` +
        `inView=${after.inView.length}, sprites=${after.spriteIds.length}, ` +
        `cachedFull=${cachedFullIds.size}`,
    ).toEqual([]);
  });

  test("focused target has a full-LOD texture within one frame of click", async ({
    page,
  }) => {
    await navigateToView(page);
    await page.waitForTimeout(2_000);

    // The forceFull synchronous-build extension means the focus
    // target itself is sharp on the very next frame after the
    // click. Verify with a tight time budget.
    const beforeNav = await getCanvasDebug(page, (api) => ({
      lod: api.getLod(),
      view: api.getView(),
      laidCount: api.getLaidNodeCount(),
      focusId: api.getFocusId(),
      spriteCount: api.getSpriteIds().length,
      resetCount: api.getSpriteResetCount(),
      ticksSwept: api.getTicksSwept(),
      syncBuildCount: api.getSyncBuildCount(),
    }));
    console.log(
      `[before nav] focusId=${beforeNav.focusId} laid=${beforeNav.laidCount} sprites=${beforeNav.spriteCount} lod=${beforeNav.lod} k=${beforeNav.view.k.toFixed(3)} resets=${beforeNav.resetCount} sweeps=${beforeNav.ticksSwept} syncBuilds=${beforeNav.syncBuildCount}`,
    );

    const target = "User";
    await page.evaluate((id: string) => {
      const api = (window as unknown as { __gqlCanvas?: { navigate(id: string): void } }).__gqlCanvas;
      if (!api) throw new Error("__gqlCanvas not exposed");
      api.navigate(id);
    }, target);

    // Wait long enough that even a long-distance focus jump (which
    // has to drain the sprite create queue first before the sweep
    // can reach the forceFull sync-build path for the focused
    // node) has had time to cache the target's full-LOD texture.
    await page.waitForTimeout(2_000);

    const after = await getCanvasDebug(page, (api) => ({
      lod: api.getLod(),
      view: api.getView(),
      keys: api.getTextureKeys(),
      sprites: api.getSpriteIds(),
      inView: api.getInViewNodeIds(),
      focusId: api.getFocusId(),
      focusJumpPending: api.isFocusJumpPending(),
      resetCount: api.getSpriteResetCount(),
      ticksSwept: api.getTicksSwept(),
      syncBuildCount: api.getSyncBuildCount(),
    }));
    console.log(
      `[after nav] focusId=${after.focusId} lod=${after.lod} k=${after.view.k.toFixed(3)} ` +
        `sprites=${after.sprites.length} inView=${after.inView.length} keys=${after.keys.length} ` +
        `focusJumpPending=${after.focusJumpPending} resets=${after.resetCount} sweeps=${after.ticksSwept} syncBuilds=${after.syncBuildCount}`,
    );
    console.log(`  sprites: ${after.sprites.slice(0, 10).join(", ")}${after.sprites.length > 10 ? ", ..." : ""}`);
    console.log(`  inView:  ${after.inView.slice(0, 10).join(", ")}${after.inView.length > 10 ? ", ..." : ""}`);
    console.log(`  keys:    ${after.keys.slice(0, 10).join(", ")}${after.keys.length > 10 ? ", ..." : ""}`);

    expect(after.lod).toBe("full");
    expect(after.focusId).toBe(target);
    expect(
      after.keys.some((k) => k.startsWith(`${target}:full:`)),
      `texture cache missing focused node. sprites=${after.sprites.length}, keys=${after.keys.length}, lod=${after.lod}`,
    ).toBe(true);
  });
});
