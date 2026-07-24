import { serve } from "bun";
import { join } from "path";
import index from "./index.html";
import setupLocatorUI from "@locator/runtime";

if (process.env.NODE_ENV === "development") {
  setupLocatorUI();
}

// Build id stamped into the service worker so each deploy ships a
// byte-different sw.js (mirrors build.ts). Falls back to "dev" when git
// isn't available; the SW is skipped on localhost anyway.
const BUILD_ID =
  Bun.spawnSync(["git", "rev-parse", "--short=6", "HEAD"]).stdout.toString().trim() || "dev";
/**
 * Bundle the layout worker on demand. Bun's HTML-entrypoint bundler
 * doesn't rewrite `new Worker(new URL(...))` into a separate emitted
 * chunk, so we build the worker ourselves and serve it at a fixed URL.
 * Dev-mode builds are cheap (~tens of ms) and uncached so edits to the
 * worker are picked up on the next load.
 */
async function buildLayoutWorker(): Promise<Response> {
  const result = await Bun.build({
    entrypoints: [join(import.meta.dir, "lib/layout-worker.ts")],
    target: "browser",
    format: "esm",
    minify: process.env.NODE_ENV === "production",
  });
  if (!result.success) {
    const msg = result.logs.map((l) => String(l)).join("\n");
    return new Response(`// worker build failed:\n${msg}`, {
      status: 500,
      headers: { "content-type": "application/javascript; charset=utf-8" },
    });
  }
  const output = result.outputs[0];
  if (!output) {
    return new Response("// worker produced no output", {
      status: 500,
      headers: { "content-type": "application/javascript; charset=utf-8" },
    });
  }
  const source = await output.text();
  return new Response(source, {
    headers: {
      "content-type": "application/javascript; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

const server = serve({
  port: Number(process.env.PORT) || 3000,
  routes: {
    // Worker bundle — kept at a fixed URL so `new Worker("/layout-worker.js")`
    // works in both dev and prod.
    "/layout-worker.js": () => buildLayoutWorker(),

    // PWA static files. Served verbatim from src/ at fixed root URLs so
    // the manifest, icons, and service worker resolve the same in dev
    // and prod (build.ts copies these into dist/). The SW is served at
    // root so its default scope covers the whole app.
    "/manifest.webmanifest": () =>
      new Response(Bun.file(join(import.meta.dir, "manifest.webmanifest")), {
        headers: { "content-type": "application/manifest+json; charset=utf-8" },
      }),
    "/sw.js": async () =>
      new Response(
        (await Bun.file(join(import.meta.dir, "sw.js")).text()).replaceAll(
          "__BUILD_ID__",
          BUILD_ID,
        ),
        {
          headers: {
            "content-type": "application/javascript; charset=utf-8",
            // Let the SW control the whole origin and never be stale.
            "service-worker-allowed": "/",
            "cache-control": "no-cache",
          },
        },
      ),
    "/icon-192.png": () => new Response(Bun.file(join(import.meta.dir, "icon-192.png"))),
    "/icon-512.png": () => new Response(Bun.file(join(import.meta.dir, "icon-512.png"))),

    "/api/hello": {
      async GET(req) {
        return Response.json({
          message: "Hello, world!",
          method: "GET",
        });
      },
      async PUT(req) {
        return Response.json({
          message: "Hello, world!",
          method: "PUT",
        });
      },
    },

    "/api/hello/:name": async (req) => {
      const name = req.params.name;
      return Response.json({
        message: `Hello, ${name}!`,
      });
    },

    // Serve index.html for all unmatched routes. Keep this last so
    // specific routes above take precedence.
    "/*": index,
  },

  development: process.env.NODE_ENV !== "production" && {
    // Enable browser hot reloading in development
    hmr: true,

    // Echo console logs from the browser to the server
    console: true,
  },
});

console.log(`🚀 Server running at ${server.url}`);
