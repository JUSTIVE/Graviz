/**
 * Graviz service worker — offline app shell + runtime asset caching.
 *
 * The build emits content-hashed chunks whose names aren't known ahead
 * of time, so instead of a fixed precache list this caches at runtime:
 *   - navigations are network-first (so a fresh deploy is picked up)
 *     with the cached "/" as the offline fallback;
 *   - same-origin GET assets (hashed JS/CSS, icons, the worker bundle)
 *     are cache-first, populated on first fetch.
 * CACHE_VERSION embeds the build id: the placeholder token below is
 * replaced with the commit hash at build/serve time (build.ts, index.ts),
 * so every deploy ships a byte-different worker. The browser then installs
 * the new worker and `activate` drops every older cache. The token stays a
 * valid string if left unreplaced (e.g. dev, where the SW is skipped).
 */
const CACHE_VERSION = "graviz-__BUILD_ID__";
const APP_SHELL = ["/", "/manifest.webmanifest", "/icon-192.png", "/icon-512.png"];

self.addEventListener("install", (event) => {
  // Don't skipWaiting() here — the new worker stays in `waiting` until the
  // page's update prompt tells it to (SKIP_WAITING message). The very first
  // install has no controller, so it activates immediately regardless.
  event.waitUntil(caches.open(CACHE_VERSION).then((cache) => cache.addAll(APP_SHELL)));
});

// The page's "new version" prompt posts this when the user opts to update.
self.addEventListener("message", (event) => {
  if (event.data && event.data.type === "SKIP_WAITING") self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((k) => k !== CACHE_VERSION).map((k) => caches.delete(k))),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const req = event.request;
  if (req.method !== "GET") return;

  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  // The API is dynamic — never serve it from cache.
  if (url.pathname.startsWith("/api/")) return;

  // Navigations: network-first so new deploys load; fall back to the
  // cached shell when offline.
  if (req.mode === "navigate") {
    event.respondWith(
      // no-store: bypass the HTTP cache so a stale index.html can't drag in
      // the old hashed chunk names (which cache-first would then serve).
      fetch(req, { cache: "no-store" })
        .then((res) => {
          const copy = res.clone();
          caches.open(CACHE_VERSION).then((cache) => cache.put("/", copy));
          return res;
        })
        .catch(() => caches.match("/").then((r) => r || Response.error())),
    );
    return;
  }

  // Everything else same-origin: cache-first, populate on miss.
  event.respondWith(
    caches.match(req).then(
      (cached) =>
        cached ||
        fetch(req).then((res) => {
          if (res && res.ok && res.type === "basic") {
            const copy = res.clone();
            caches.open(CACHE_VERSION).then((cache) => cache.put(req, copy));
          }
          return res;
        }),
    ),
  );
});
