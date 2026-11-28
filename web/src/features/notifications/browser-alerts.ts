import { holdDesktopChannel } from "./desktop-channel";

const permissionChanged = new EventTarget();

export function browserAlertPermission(): NotificationPermission | "unsupported" {
  // The development preview can be read-only and can proxy a different service.
  return !import.meta.env.DEV &&
    window.isSecureContext &&
    "Notification" in window &&
    "serviceWorker" in navigator &&
    "ServiceWorkerRegistration" in window &&
    "showNotification" in ServiceWorkerRegistration.prototype
    ? Notification.permission
    : "unsupported";
}

export function onAlertPermission(listener: () => void) {
  permissionChanged.addEventListener("change", listener);
  return () => permissionChanged.removeEventListener("change", listener);
}

export async function requestAlertPermission() {
  try {
    await Notification.requestPermission();
  } finally {
    permissionChanged.dispatchEvent(new Event("change"));
  }
}

export function startBrowserAlerts(openNotification: (id: string) => void) {
  if (browserAlertPermission() === "unsupported") return () => {};
  const life = new AbortController();
  const { signal } = life;
  let paused = false;
  let leader: AbortController | undefined;
  function stopChannel() {
    leader?.abort();
    leader = undefined;
  }
  async function connect(channel: AbortSignal) {
    const registration = await navigator.serviceWorker.ready;
    if (channel.aborted) return;
    if (navigator.locks) {
      // Hidden tabs can receive alerts. One tab per browser profile owns delivery.
      await navigator.locks.request("reviewq-desktop-notifications", { signal: channel }, () =>
        holdDesktopChannel(registration, channel),
      );
    } else {
      await holdDesktopChannel(registration, channel);
    }
  }
  function sync() {
    if (paused || browserAlertPermission() !== "granted") return stopChannel();
    if (leader) return;
    const channel = new AbortController();
    leader = channel;
    void connect(channel.signal)
      .catch(() => {})
      .finally(() => {
        if (leader === channel) leader = undefined;
      });
  }
  function changed() {
    permissionChanged.dispatchEvent(new Event("change"));
  }
  function resume() {
    paused = false;
    changed();
  }
  function suspend() {
    paused = true;
    stopChannel();
  }
  // The app registers the worker at start. Accept messages only from the registration's active
  // worker, read at message time: the browser updates `active` when a new worker takes over.
  let registration: ServiceWorkerRegistration | undefined;
  void navigator.serviceWorker.ready.then((ready) => {
    registration = ready;
  });
  navigator.serviceWorker.addEventListener(
    "message",
    (event: MessageEvent<unknown>) => {
      const data = event.data;
      if (
        !registration?.active ||
        event.source !== registration.active ||
        !data ||
        typeof data !== "object" ||
        !("type" in data)
      )
        return;
      if (data.type === "notification-ready") {
        event.ports[0]?.postMessage(true);
        return;
      }
      if (
        data.type !== "open-notification" ||
        !("notificationId" in data) ||
        typeof data.notificationId !== "string" ||
        !data.notificationId ||
        data.notificationId.length > 256 ||
        /\p{Cc}/u.test(data.notificationId)
      )
        return;
      openNotification(data.notificationId);
      event.ports[0]?.postMessage(data.notificationId);
    },
    { signal },
  );
  permissionChanged.addEventListener("change", sync, { signal });
  window.addEventListener("focus", changed, { signal });
  window.addEventListener("pagehide", suspend, { signal });
  window.addEventListener("pageshow", resume, { signal });
  document.addEventListener("freeze", suspend, { signal });
  document.addEventListener("resume", resume, { signal });
  void navigator.permissions
    ?.query({ name: "notifications" })
    .then((permission) => permission.addEventListener("change", changed, { signal }))
    .catch(() => {});
  sync();
  return () => {
    life.abort();
    stopChannel();
  };
}
