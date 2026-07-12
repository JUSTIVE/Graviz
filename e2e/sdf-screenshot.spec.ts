import { test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Visual smoke for the SDF text experiment — not a regression gate.
 * Captures the canvas at full LOD with `?sdf=1` (default on) and
 * `?sdf=0` (legacy raster text) so the two renderers can be diffed by
 * eye. Screenshots land in e2e/__screenshots__/.
 */

const here = dirname(fileURLToPath(import.meta.url));
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
  getLaidNodeCount(): number;
  navigate(id: string): void;
}

declare global {
  interface Window {
    __gqlCanvas?: CanvasDebug;
  }
}

for (const sdf of ["1", "0"] as const) {
  test(`canvas full-LOD screenshot (sdf=${sdf})`, async ({ page }) => {
    test.setTimeout(240_000);
    await seedHistory(page, largeSchema);
    // The SDF flag is read once at module load from location.search,
    // so it only needs to be on the entry URL.
    await page.goto(`/?sdf=${sdf}`);
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
    await page.waitForFunction(
      () => (window.__gqlCanvas?.getLaidNodeCount() ?? 0) > 0,
      undefined,
      { timeout: 90_000 },
    );
    // Jump to a mid-size type so the view sits at full LOD.
    await page.evaluate(() => window.__gqlCanvas!.navigate("Repository"));
    await page.waitForFunction(
      () => window.__gqlCanvas?.getLod() === "full",
      undefined,
      { timeout: 30_000 },
    );
    await page.waitForTimeout(2500); // let progressive builds drain
    await page.screenshot({
      path: join(here, "__screenshots__", `sdf-${sdf}-full.png`),
    });
    // Zoom in hard — where the old renderer needed a high-DPR bucket
    // rebuild and SDF should stay crisp for free.
    const canvas = page.locator("canvas").first();
    const box = (await canvas.boundingBox())!;
    const cx = box.x + box.width / 2;
    const cy = box.y + box.height / 2;
    await page.mouse.move(cx, cy);
    for (let i = 0; i < 6; i++) {
      await page.mouse.wheel(0, -400);
      await page.waitForTimeout(150);
    }
    await page.waitForTimeout(2000);
    await page.screenshot({
      path: join(here, "__screenshots__", `sdf-${sdf}-zoomed.png`),
    });
  });
}
