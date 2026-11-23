import { readBrowserProof } from "../../api/client";

type Offer = { notificationId: string; title: string; body: string };

export async function holdDesktopChannel(
  registration: ServiceWorkerRegistration,
  signal: AbortSignal,
) {
  let backoff = 750;
  while (!signal.aborted && Notification.permission === "granted") {
    const started = Date.now();
    try {
      const proof = await readBrowserProof(AbortSignal.any([signal, AbortSignal.timeout(8000)]));
      signal.throwIfAborted();
      const code = await receive(registration, proof, signal);
      if (code === 1008 || code === 1000) return;
    } catch {
      if (signal.aborted) return;
    }
    if (Date.now() - started > 30_000) backoff = 750;
    await new Promise<void>((resolve) => {
      const timer = window.setTimeout(end, backoff);
      function end() {
        clearTimeout(timer);
        signal.removeEventListener("abort", end);
        resolve();
      }
      signal.addEventListener("abort", end, { once: true });
      if (signal.aborted) end();
    });
    backoff = Math.min(backoff * 2, 30_000);
  }
}

function receive(
  registration: ServiceWorkerRegistration,
  proof: string,
  signal: AbortSignal,
): Promise<number> {
  return new Promise((resolve) => {
    const socket = new WebSocket("/api/v1/notifications/desktop/channel");
    let offer: Offer | undefined;
    function close() {
      socket.close();
    }
    function send(type: string, notificationId: string) {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(JSON.stringify({ type, notificationId }));
      }
    }
    async function message(raw: unknown) {
      if (typeof raw !== "string" || raw.length > 4096) return close();
      const data: unknown = JSON.parse(raw);
      if (typeof data !== "object" || !data || !("type" in data)) return close();
      if (data.type === "ready") return;
      if (!("notificationId" in data) || typeof data.notificationId !== "string") return close();
      const id = data.notificationId;
      if (data.type === "offer") {
        if (
          Notification.permission !== "granted" ||
          !registration.active ||
          !("body" in data) ||
          typeof data.body !== "string" ||
          !("title" in data) ||
          typeof data.title !== "string"
        ) {
          send("decline", id);
          return;
        }
        offer = { notificationId: id, body: data.body, title: data.title };
        send("accept", id);
      } else if (data.type === "cancel" && offer?.notificationId === id) {
        offer = undefined;
      } else if (data.type === "display" && offer?.notificationId === id) {
        const current = offer;
        offer = undefined;
        try {
          await registration.showNotification(current.title, {
            body: current.body,
            tag: "reviewq:" + id,
            icon: "/icon.svg",
            data: { notificationId: id },
          });
          send("displayed", id);
        } catch {
          send("failed", id);
        }
      }
    }
    signal.addEventListener("abort", close, { once: true });
    socket.onopen = () => {
      if (signal.aborted) return close();
      socket.send(JSON.stringify({ type: "hello", csrfToken: proof }));
    };
    socket.onmessage = (event: MessageEvent<unknown>) => void message(event.data).catch(close);
    socket.onclose = (event) => {
      signal.removeEventListener("abort", close);
      resolve(event.code);
    };
    if (signal.aborted) close();
  });
}
