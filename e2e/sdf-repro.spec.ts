import { expect, test, type Page } from "@playwright/test";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Regression coverage for SDF text loss on huge CJK-heavy nodes.
 *
 * Two historical failure modes, both reproduced with this synthetic
 * schema (one 600-field type, Korean descriptions, thousands of
 * unique hangul glyphs):
 * - Buffer undercount: the quad pre-count skipped spaces while the
 *   font reported a non-zero bounding box for them, so the subarray
 *   views silently clamped and the mesh tail (bottom rows) vanished.
 * - Giant single draw: one Mesh with >65k vertices truncated on some
 *   drivers (ANGLE→Metal), fixed by chunking at MAX_QUADS_PER_MESH.
 */

const here = dirname(fileURLToPath(import.meta.url));

// Synthetic schema: lots of unique hangul glyphs + one huge type.
const sdl = (() => {
  const syllable = (i: number) => String.fromCharCode(0xac00 + (i % 11172));
  let s = "type Query {\n  big: BigType\n";
  for (let t = 0; t < 30; t++) s += `  t${t}: Filler${t}\n`;
  s += "}\n";
  let g = 0;
  for (let t = 0; t < 30; t++) {
    s += `type Filler${t} {\n`;
    for (let f = 0; f < 10; f++) {
      let desc = "";
      for (let c = 0; c < 10; c++) desc += syllable(g++ * 7);
      s += `  """${desc}"""\n  field${f}: String\n`;
    }
    s += "}\n";
  }
  s += "type BigType {\n";
  for (let f = 0; f < 600; f++) {
    let desc = "설명 ";
    for (let c = 0; c < 100; c++) desc += syllable((f * 31 + c * 3) % 800);
    s += `  """${desc}"""\n  someLongFieldName${f}: SomeReturnType${f % 5}\n`;
  }
  s += "}\n";
  for (let r = 0; r < 5; r++) s += `type SomeReturnType${r} { id: ID }\n`;
  return s;
})();

function hashSdl(s0: string): string {
  const s = s0.trim();
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  return h.toString(36);
}

interface CanvasDebug {
  getLod(): string;
  getLaidNodeCount(): number;
  navigate(id: string): void;
}
type DebugWindow = { __gqlCanvas?: CanvasDebug };

test.use({ deviceScaleFactor: 2, viewport: { width: 1280, height: 800 } });

test("SDF text on a huge CJK-heavy node", async ({ page }) => {
  test.setTimeout(180_000);
  const warns: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "warning" || m.type() === "error") warns.push(m.text());
  });
  await page.addInitScript((entries) => {
    localStorage.setItem("gompassql:history", JSON.stringify(entries));
  }, [{ hash: hashSdl(sdl), sdl, name: "repro.graphql", createdAt: Date.now(), updatedAt: Date.now() }]);
  await page.goto("/");
  await page.getByRole("button", { name: /Recent schemas/i }).click({ timeout: 10_000 });
  await page.getByRole("button", { name: /repro\.graphql/i }).click({ timeout: 10_000 });
  await page.getByRole("button", { name: /^Visualize$/ }).click({ timeout: 10_000 });
  await page.waitForURL(/\/view/, { timeout: 30_000 });
  await page.getByText(/Laying out/i).first().waitFor({ state: "hidden", timeout: 60_000 }).catch(() => {});
  await page.waitForFunction(
    () => ((window as unknown as DebugWindow).__gqlCanvas?.getLaidNodeCount() ?? 0) > 0,
    undefined,
    { timeout: 60_000 },
  );
  // Visit fillers first so the atlas absorbs their glyphs, then the
  // big node.
  for (const id of ["Filler0", "Filler10", "Filler20", "Filler29"]) {
    await page.evaluate(
      (nid) => (window as unknown as DebugWindow).__gqlCanvas!.navigate(nid),
      id,
    );
    await page.waitForTimeout(800);
  }
  await page.evaluate(() => (window as unknown as DebugWindow).__gqlCanvas!.navigate("BigType"));
  await page.waitForTimeout(3000);
  await page.screenshot({ path: join(here, "__screenshots__", "repro-bigtype-top.png") });
  // Scroll down inside the node.
  const canvas = page.locator("canvas").first();
  const box = (await canvas.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  // Zoom in to full LOD then pan down the card.
  for (let i = 0; i < 3; i++) {
    await page.mouse.wheel(0, -400);
    await page.waitForTimeout(150);
  }
  await page.waitForTimeout(1500);
  await page.screenshot({ path: join(here, "__screenshots__", "repro-bigtype-zoom.png") });
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2 - 500, { steps: 10 });
  await page.mouse.up();
  await page.waitForTimeout(1500);
  await page.screenshot({ path: join(here, "__screenshots__", "repro-bigtype-mid.png") });
  // Frame the LAST field via a tree-panel click — its quads sit past
  // any single-draw truncation boundary on a >70k-quad node.
  const lastField = page.getByText("someLongFieldName599", { exact: true }).first();
  await lastField.scrollIntoViewIfNeeded();
  await lastField.click();
  await page.waitForTimeout(2500);
  await page.screenshot({ path: join(here, "__screenshots__", "repro-bigtype-last-field.png") });
  const meshInfo = await page.evaluate(() =>
    (window as unknown as {
      __gqlCanvas?: {
        getTextMeshInfo(): {
          id: string;
          chunks: { indices: number; positions: number }[];
        }[];
      };
    }).__gqlCanvas!.getTextMeshInfo(),
  );
  console.log("text mesh info:", JSON.stringify(meshInfo));
  console.log("console warnings/errors:", JSON.stringify(warns, null, 2));
  // Every chunk's buffers must agree on quad count — a mismatch means
  // the pre-count under-allocated and subarray views clamped (the bug
  // that made bottom rows vanish on huge nodes).
  const big = meshInfo.find((m) => m.id === "BigType");
  expect(big).toBeTruthy();
  expect(big!.chunks.length).toBeGreaterThan(1);
  for (const c of big!.chunks) {
    expect(c.indices / 6).toBe(c.positions / 8);
  }
});
