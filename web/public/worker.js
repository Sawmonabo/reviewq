// This worker handles notification clicks and shows the stopped page when a page load cannot reach
// ReviewQ. Every other request goes to the network. Only the app writes the stopped-page cache.
const STOPPED_CACHE = "reviewq-stopped";

self.addEventListener("install", (event) =>
  event.waitUntil(Promise.all([bypassWorker(event), self.skipWaiting()])),
);
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("fetch", (event) => {
  if (event.request.mode !== "navigate") return;
  event.respondWith(
    fetch(event.request).catch(
      async () =>
        (await caches.match("/stopped.html", { cacheName: STOPPED_CACHE })) ?? Response.error(),
    ),
  );
});

// Where the browser supports static routes, API and asset requests never start the worker.
async function bypassWorker(event) {
  if (!event.addRoutes) return;
  try {
    await event.addRoutes(
      ["/api/*", "/assets/*"].map((pathname) => ({
        condition: { urlPattern: new URLPattern({ pathname }) },
        source: "network",
      })),
    );
  } catch {
    // The fetch handler returns at once for these requests.
  }
}

self.addEventListener("notificationclick", (event) => {
  const id = event.notification.data?.notificationId;
  if (typeof id !== "string" || !id || id.length > 256 || /\p{Cc}/u.test(id)) return;
  event.notification.close();
  event.waitUntil(openInboxItem(id));
});

async function openInboxItem(id) {
  const url = new URL("/", self.location.origin);
  url.searchParams.set("view", "notifications");
  url.searchParams.set("notificationId", id);
  url.hash = "notification-" + encodeURIComponent(id);
  const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  // matchAll returns the most recently focused window first. Client.url can
  // still contain its initial query after the dashboard uses pushState.
  const candidates = windows.filter((client) => {
    const initial = new URL(client.url);
    return (
      initial.origin === url.origin &&
      (initial.pathname === "/" || initial.pathname === "/index.html") &&
      (client.frameType === "top-level" || client.frameType === "auxiliary")
    );
  });
  // Ask in parallel. A tab opened on an earlier release may hold an unsent draft
  // and have no click handler. Reloading it to deliver this link would erase the draft.
  const available = candidates.map((client) => ask(client, { type: "notification-ready" }, true));
  for (const [index, client] of candidates.entries()) {
    if (!(await available[index])) continue;
    try {
      await client.focus();
    } catch {
      if (!(await self.clients.get(client.id))) continue;
    }
    const handled = await ask(client, { type: "open-notification", notificationId: id }, id);
    if (handled || (await self.clients.get(client.id))) return;
  }
  await self.clients.openWindow(url.href);
}

function ask(client, message, expected) {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    function finish(handled) {
      clearTimeout(deadline);
      channel.port1.close();
      resolve(handled);
    }
    const deadline = setTimeout(() => finish(false), 1500);
    channel.port1.onmessage = ({ data }) => finish(data === expected);
    try {
      client.postMessage(message, [channel.port2]);
    } catch {
      finish(false);
    }
  });
}
