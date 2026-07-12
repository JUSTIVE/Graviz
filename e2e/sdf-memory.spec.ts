import { test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Memory measurement for the SDF experiment — logs estimated GPU
 * texture bytes held by the node texture cache in both renderer
 * modes at an identical deep-zoom viewpoint. Not a regression gate.
 */

const here = dirname(fileURLToPath(import.meta.url));
const largeSchema = readFileSync(join(here, "..", "schema.docs.graphql"), "utf8");

function hashSdl(sdl: string): string {
  const s = sdl.trim();
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  return h.toString(36);
}

async function seedHistory(page: Page, sdl: string) {
  await page.addInitScript((entries) => {
    localStorage.setItem("gompassql:history", JSON.stringify(entries));
  }, [{ hash: hashSdl(sdl), sdl, name: "github.graphql", createdAt: Date.now(), updatedAt: Date.now() }]);
}

interface CanvasDebug {
  getLod(): string;
  getLaidNodeCount(): number;
  getTextureKeys(): string[];
  getNodeDimensions(): { id: string; w: number; h: number }[];
  navigate(id: string): void;
}
/** Window shape inside page.evaluate contexts (cast per callback —
 *  evaluate functions are serialized, so no outer helpers). */
type DebugWindow = { __gqlCanvas?: CanvasDebug };

for (const sdf of ["1", "0"] as const) {
  test(`texture memory at deep zoom (sdf=${sdf})`, async ({ page }) => {
    test.setTimeout(240_000);
    await seedHistory(page, largeSchema);
    await page.goto(`/?sdf=${sdf}`);
    await page.getByRole("button", { name: /Recent schemas/i }).click({ timeout: 10_000 });
    await page.getByRole("button", { name: /github\.graphql/i }).click({ timeout: 10_000 });
    await page.getByRole("button", { name: /^Visualize$/ }).click({ timeout: 10_000 });
    await page.waitForURL(/\/view/, { timeout: 30_000 });
    await page.getByText(/Laying out/i).first().waitFor({ state: "hidden", timeout: 120_000 }).catch(() => {});
    await page.waitForFunction(
      () => ((window as unknown as DebugWindow).__gqlCanvas?.getLaidNodeCount() ?? 0) > 0,
      undefined,
      { timeout: 90_000 },
    );
    await page.evaluate(() =>
      (window as unknown as DebugWindow).__gqlCanvas!.navigate("Repository"),
    );
    await page.waitForFunction(
      () => (window as unknown as DebugWindow).__gqlCanvas?.getLod() === "full",
      undefined,
      { timeout: 30_000 },
    );
    await page.waitForTimeout(2500);

    const measure = async (label: string) => {
      const r = await page.evaluate(() => {
        const api = (window as unknown as DebugWindow).__gqlCanvas!;
        const dims = new Map(api.getNodeDimensions().map((d) => [d.id, d]));
        let bytes = 0;
        let count = 0;
        const dprs = new Map<string, number>();
        for (const key of api.getTextureKeys()) {
          const [id, lod, dprStr] = key.split(":");
          if (lod !== "full") continue;
          const d = dims.get(id!);
          if (!d) continue;
          const dpr = parseFloat(dprStr!);
          bytes += Math.ceil(d.w * dpr) * Math.ceil(d.h * dpr) * 4;
          count++;
          dprs.set(dprStr!, (dprs.get(dprStr!) ?? 0) + 1);
        }
        return { bytes, count, dprs: [...dprs.entries()] };
      });
      console.log(
        `[mem sdf=${label}] full-LOD textures: ${r.count}, est ${(r.bytes / 1024 / 1024).toFixed(1)} MB, dpr buckets: ${JSON.stringify(r.dprs)}`,
      );
    };

    await measure(`${sdf} @full-lod-fit`);

    const canvas = page.locator("canvas").first();
    const box = (await canvas.boundingBox())!;
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    for (let i = 0; i < 6; i++) {
      await page.mouse.wheel(0, -400);
      await page.waitForTimeout(150);
    }
    await page.waitForTimeout(2500);
    await measure(`${sdf} @deep-zoom`);
  });
}
