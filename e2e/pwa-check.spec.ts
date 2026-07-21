import { test, expect, type Page } from "@playwright/test";

test("PWA: manifest + icons served, SW registers and serves offline", async ({ page, context }) => {
  test.setTimeout(120_000);

  // Manifest is valid and links the required icons.
  const mani = await page.request.get("/manifest.webmanifest");
  expect(mani.ok()).toBeTruthy();
  const m = await mani.json();
  expect(m.name).toContain("Graviz");
  expect(m.display).toBe("standalone");
  const sizes = (m.icons as { sizes: string }[]).map((i) => i.sizes);
  expect(sizes).toContain("192x192");
  expect(sizes).toContain("512x512");
  for (const p of ["/icon-192.png", "/icon-512.png", "/sw.js"]) {
    const r = await page.request.get(p);
    expect(r.ok(), `${p} should serve`).toBeTruthy();
  }

  // Opt into SW registration on localhost (normally skipped for HMR).
  await page.addInitScript(() => localStorage.setItem("pwa:sw", "force"));
  await page.goto("/");
  // Manifest link is injected at runtime.
  await expect(page.locator('link[rel="manifest"]')).toHaveCount(1);
  // ...and the SW takes control.
  await page.waitForFunction(
    () => !!navigator.serviceWorker.controller,
    undefined,
    { timeout: 30_000 },
  );
  // Give the SW a moment to populate the runtime cache with the shell
  // + hashed assets fetched during this load.
  await page.waitForTimeout(2500);

  // Go offline and reload — the app shell must still boot from cache.
  await context.setOffline(true);
  await page.reload();
  await expect(page.getByRole("link", { name: "Graviz" })).toBeVisible({ timeout: 15_000 });
  await context.setOffline(false);
});
