import { test, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * Locks in the "Show descriptions" toggle behavior on the graph
 * canvas. Toggling re-runs dot layout with the per-row/header
 * heights bumped from `ROW_H` / `HEADER_H` to their
 * `_WITH_DESC` counterparts so the type description + per-field
 * descriptions render inline. Verified by reading the debug
 * hook (`window.__gqlCanvas.getNodeDimensions`) before and after
 * the toggle click.
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

interface NodeDim {
  id: string;
  rowH: number;
  headerH: number;
  w: number;
  h: number;
}

async function readDimensions(page: Page): Promise<NodeDim[]> {
  return await page.evaluate(() => {
    const api = (
      window as unknown as {
        __gqlCanvas?: { getNodeDimensions(): NodeDim[] };
      }
    ).__gqlCanvas;
    if (!api) throw new Error("__gqlCanvas not exposed");
    return api.getNodeDimensions();
  });
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
  await page
    .getByText(/Laying out/i)
    .first()
    .waitFor({ state: "hidden", timeout: 120_000 })
    .catch(() => {});
  await page.locator("canvas").first().waitFor({ timeout: 15_000 });
}

test.describe("Show-descriptions toggle on the graph", () => {
  test.setTimeout(240_000);

  test("toggling collapses rowH / headerH on every laid node", async ({
    page,
  }) => {
    await navigateToView(page);
    await page.waitForTimeout(2_000);

    const before = await readDimensions(page);
    expect(before.length).toBeGreaterThan(0);
    // Sanity: "Show descriptions" is ON by default, so every node
    // starts at the with-description ROW_H (26) and HEADER_H (56).
    for (const n of before) {
      expect(n.rowH).toBe(26);
      expect(n.headerH).toBe(56);
    }
    const baseHeights = new Map(before.map((n) => [n.id, n.h]));

    // Click the toggle in the filter chip cluster. The filter
    // chips share an opacity-40 panel that the canvas reveals on
    // hover — Playwright clicks through that fine, but using
    // `force` short-circuits the visibility heuristic just in case
    // CI renders the panel differently.
    await page
      .getByRole("button", { name: /Show descriptions/i })
      .click({ timeout: 10_000, force: true });

    // Layout re-runs on toggle — wait for the overlay to clear.
    await page
      .getByText(/Laying out/i)
      .first()
      .waitFor({ state: "hidden", timeout: 60_000 })
      .catch(() => {});
    await page.waitForTimeout(2_000);

    const after = await readDimensions(page);
    expect(after.length).toBeGreaterThan(0);
    // Every node should now carry the base (no-description) heights.
    for (const n of after) {
      expect(n.rowH).toBe(14);
      expect(n.headerH).toBe(42);
    }
    // And the total card height of every body-bearing node should
    // be strictly smaller than before (or equal for empty Scalars,
    // which have no rows to shrink).
    let anySmaller = false;
    for (const n of after) {
      const prevH = baseHeights.get(n.id);
      if (prevH == null) continue;
      expect(n.h).toBeLessThanOrEqual(prevH);
      if (n.h < prevH) anySmaller = true;
    }
    expect(anySmaller).toBe(true);
  });

  test("toggling twice restores rowH / headerH to the default sizes", async ({
    page,
  }) => {
    await navigateToView(page);
    await page.waitForTimeout(2_000);
    const toggle = page.getByRole("button", { name: /Show descriptions/i });

    // Toggle OFF (descriptions are on by default)
    await toggle.click({ timeout: 10_000, force: true });
    await page
      .getByText(/Laying out/i)
      .first()
      .waitFor({ state: "hidden", timeout: 60_000 })
      .catch(() => {});
    await page.waitForTimeout(1_000);

    // Toggle back ON
    await toggle.click({ timeout: 10_000, force: true });
    await page
      .getByText(/Laying out/i)
      .first()
      .waitFor({ state: "hidden", timeout: 60_000 })
      .catch(() => {});
    await page.waitForTimeout(1_000);

    const dims = await readDimensions(page);
    expect(dims.length).toBeGreaterThan(0);
    for (const n of dims) {
      expect(n.rowH).toBe(26);
      expect(n.headerH).toBe(56);
    }
  });
});
