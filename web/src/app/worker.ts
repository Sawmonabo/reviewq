const STOPPED_CACHE = "reviewq-stopped";

// The worker serves this cached page when a page load cannot reach ReviewQ. Each start stores a
// fresh copy, so it always matches the running build and its start button.
export function startWorker() {
  if (!import.meta.env.PROD || !("serviceWorker" in navigator)) return;
  navigator.serviceWorker
    .register("/worker.js", { scope: "/" })
    .then(() => caches.open(STOPPED_CACHE))
    .then((cache) => cache.add("/stopped.html"))
    .catch(() => {});
}
