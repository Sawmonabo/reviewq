import type { ResourceKeyDto } from "./generated/contracts";
import { isInvalidateEvent, isResetEvent } from "./generated/validators.mjs";

export type EventConnectionStatus = {
  readonly state: "ready" | "reconnecting";
  readonly lastConnectedAt?: number | undefined;
};

type RefreshReason = "invalidate" | "reset" | "visible";

type Listener = {
  readonly keys: readonly ResourceKeyDto[];
  readonly notify: (reason: RefreshReason) => void;
  snapshot?: string;
  delivered?: string;
};
const listeners = new Set<Listener>();
let stream: EventSource | undefined;
let cursor: string | undefined;
let replayFrom: string | undefined;
let reconnect: number | undefined;
let retryDelay = 500;
let lastConnectedAt: number | undefined;
let status: EventConnectionStatus["state"] = "ready";
let reportStatus: ((status: EventConnectionStatus) => void) | undefined;

export function watchResources(keys: readonly ResourceKeyDto[], notify: Listener["notify"]) {
  const listener: Listener = { keys, notify };
  listeners.add(listener);
  return {
    snapshot(value: string) {
      if (!/^\d+$/.test(value)) return;
      const first = listener.snapshot === undefined;
      listener.snapshot = value;
      if (first && stream && cursor && BigInt(value) < BigInt(cursor)) {
        if (!replayFrom || BigInt(value) < BigInt(replayFrom)) replayFrom = value;
        if (reconnect === undefined) reconnect = window.setTimeout(replay, 50);
      } else observeSnapshot(value);
    },
    close() {
      listeners.delete(listener);
    },
  };
}

export function observeSnapshot(value: string) {
  if (!/^\d+$/.test(value) || stream) return;
  if (!replayFrom || BigInt(value) < BigInt(replayFrom)) replayFrom = value;
  if (reconnect === undefined) replay();
}

export function retryWorkspaceEvents() {
  if (status === "reconnecting") replay();
}

export function startWorkspaceEvents(
  onStatus: (status: EventConnectionStatus) => void,
): () => void {
  reportStatus = onStatus;
  function visibility() {
    if (document.hidden) {
      disconnect();
      return;
    }
    if (status === "reconnecting") {
      replay();
      return;
    }
    if (replayFrom !== undefined || cursor !== undefined) replay();
    for (const listener of listeners) listener.notify("visible");
  }
  function networkChanged() {
    if (document.hidden) return;
    if (status === "reconnecting") replay();
    for (const listener of listeners) listener.notify("visible");
  }
  function restore(event: PageTransitionEvent) {
    if (event.persisted) visibility();
  }
  document.addEventListener("visibilitychange", visibility);
  window.addEventListener("online", networkChanged);
  window.addEventListener("offline", networkChanged);
  window.addEventListener("pagehide", disconnect);
  window.addEventListener("pageshow", restore);
  return () => {
    document.removeEventListener("visibilitychange", visibility);
    window.removeEventListener("online", networkChanged);
    window.removeEventListener("offline", networkChanged);
    window.removeEventListener("pagehide", disconnect);
    window.removeEventListener("pageshow", restore);
    disconnect();
    cursor = undefined;
    replayFrom = undefined;
    retryDelay = 500;
    lastConnectedAt = undefined;
    status = "ready";
    reportStatus = undefined;
  };
}

function report(state: EventConnectionStatus["state"]) {
  if (status === state) return;
  status = state;
  reportStatus?.({ state, lastConnectedAt });
}

function disconnect() {
  stream?.close();
  stream = undefined;
  if (reconnect !== undefined) window.clearTimeout(reconnect);
  reconnect = undefined;
}

function replay() {
  disconnect();
  if (document.hidden) return;
  const from = replayFrom ?? cursor;
  replayFrom = undefined;
  const next = new EventSource(`/api/v1/events${from ? `?after=${encodeURIComponent(from)}` : ""}`);
  stream = next;
  next.addEventListener("open", () => {
    if (stream !== next) return;
    retryDelay = 500;
    lastConnectedAt = Date.now();
    if (status === "ready") return;
    cursor = undefined;
    // A restarted service can reuse an event position. Revalidate active snapshots
    // even if replay has no newer entries, and resume from the new generation.
    for (const entry of listeners) {
      delete entry.snapshot;
      delete entry.delivered;
      entry.notify("visible");
    }
    report("ready");
  });
  next.addEventListener("error", () => {
    if (stream !== next) return;
    disconnect();
    report("reconnecting");
    if (!document.hidden) {
      reconnect = window.setTimeout(replay, retryDelay);
      retryDelay = Math.min(15_000, retryDelay * 2);
    }
  });
  next.addEventListener("invalidate", (event: MessageEvent<string>) => {
    const body = parse(event.data);
    if (stream !== next || !isInvalidateEvent(body) || !/^\d+$/.test(event.lastEventId)) return;
    const version = BigInt(event.lastEventId);
    if (!cursor || version > BigInt(cursor)) cursor = event.lastEventId;
    for (const entry of listeners) {
      if (
        (entry.snapshot && version <= BigInt(entry.snapshot)) ||
        (entry.delivered && version <= BigInt(entry.delivered))
      )
        continue;
      if (
        body.resources.some((resource) =>
          entry.keys.some(
            (key) =>
              key.kind === resource.kind && (!key.id || !resource.id || key.id === resource.id),
          ),
        )
      ) {
        entry.delivered = event.lastEventId;
        entry.notify("invalidate");
      }
    }
  });
  next.addEventListener("reset", (event: MessageEvent<string>) => {
    if (stream !== next || !isResetEvent(parse(event.data))) return;
    disconnect();
    cursor = undefined;
    replayFrom = undefined;
    for (const entry of listeners) {
      delete entry.snapshot;
      delete entry.delivered;
      entry.notify("reset");
    }
  });
}

function parse(value: string): unknown {
  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}
