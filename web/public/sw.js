// web/build.ts rewrites the two marked lines: BUILD changes with every bundle, so the
// worker file changes byte-for-byte and browsers install it; ASSETS are the hashed files.
const BUILD = "dev"; // @build
const ASSETS = []; // @assets
const SHELL = `pizza-delivery-shell-${BUILD}`;
const FILES = ["/", "/manifest.webmanifest", "/icon.svg", "/icon-192.png", "/icon-512.png", ...ASSETS];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(SHELL)
      .then((cache) => cache.addAll(FILES))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== SHELL).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

function store(key, response) {
  if (!response.ok) return;
  const copy = response.clone();
  void caches.open(SHELL).then((cache) => cache.put(key, copy));
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== "GET" || url.origin !== self.location.origin) return;
  if (url.pathname.startsWith("/api/")) return;
  const key = request.mode === "navigate" ? "/" : url.pathname;
  if (!FILES.includes(key)) return;
  if (ASSETS.includes(key)) {
    // Content-hashed: the cached copy can never be stale, so skip the network.
    event.respondWith(
      caches.match(key).then(
        (hit) =>
          hit ??
          fetch(request).then((response) => {
            store(key, response);
            return response;
          }),
      ),
    );
    return;
  }
  // The shell and icons revalidate on every load, which is how a deploy reaches the page
  // without a hard refresh: the fresh index names the new hashed bundle.
  event.respondWith(
    fetch(request)
      .then((response) => {
        store(key, response);
        return response;
      })
      .catch(() => caches.match(key).then((hit) => hit ?? Response.error())),
  );
});

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = {};
  }
  const title = typeof data.title === "string" ? data.title : "pizza-delivery";
  const job = typeof data.job === "string" ? data.job : "";
  event.waitUntil(
    self.registration.showNotification(title, {
      body: job ? `job ${job.slice(-8).toLowerCase()}` : "",
      tag: `${job}:${data.state ?? ""}`,
      icon: "/icon-192.png",
      data: { url: typeof data.url === "string" ? data.url : "/" },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = event.notification.data?.url ?? "/";
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      for (const client of windows) {
        if ("navigate" in client) return client.navigate(url).then((c) => c && c.focus());
      }
      return self.clients.openWindow(url);
    }),
  );
});
