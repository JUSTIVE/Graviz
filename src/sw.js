/**
 * Graviz service worker — offline app shell + runtime asset caching.
 *
 * The build emits content-hashed chunks whose names aren't known ahead
 * of time, so instead of a fixed precache list this caches at runtime:
 *   - navigations are network-first (so a fresh deploy is picked up)
 *     with the cached "/" as the offline fallback;
 *   - same-origin GET assets (hashed JS/CSS, icons, the worker bundle)
 *     are cache-first, populated on first fetch.
 * Bumping CACHE_VERSION drops every older cache on activate.
 */
const CACHE_VERSION = "graviz-v1";
const APP_SHELL = ["/", "/manifest.webmanifest", "/icon-192.png", "/icon-512.png"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE_VERSION)
      .then((cache) => cache.addAll(APP_SHELL))
      .then(() => self.skipWaiting()),
  );
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
      fetch(req)
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
