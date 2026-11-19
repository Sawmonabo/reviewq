import type { ResourceKeyDto } from "./generated/contracts";
import { isInvalidateEvent, isResetEvent } from "./generated/validators.mjs";

type Listener = {
  readonly keys: readonly ResourceKeyDto[];
  readonly notify: () => void;
  snapshot?: string;
  delivered?: string;
};
const listeners = new Set<Listener>();
let stream: EventSource | undefined;
let cursor: string | undefined;
let replayFrom: string | undefined;
let reconnect: number | undefined;

export function watchResources(keys: readonly ResourceKeyDto[], notify: () => void) {
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
    close() { listeners.delete(listener); },
  };
}

export function observeSnapshot(value: string) {
  if (!/^\d+$/.test(value) || stream) return;
  if (!replayFrom || BigInt(value) < BigInt(replayFrom)) replayFrom = value;
  replay();
}

export function startWorkspaceEvents(): () => void {
  function visibility() {
    if (document.hidden) {
      disconnect();
    } else {
      replayFrom ??= cursor;
      if (replayFrom !== undefined) replay();
      for (const listener of listeners) listener.notify();
    }
  }
  document.addEventListener("visibilitychange", visibility);
  window.addEventListener("pagehide", disconnect);
  return () => {
    document.removeEventListener("visibilitychange", visibility);
    window.removeEventListener("pagehide", disconnect);
    disconnect();
    cursor = undefined;
    replayFrom = undefined;
  };
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
  const from = replayFrom;
  replayFrom = undefined;
  const next = new EventSource(`/api/v1/events${from ? `?after=${encodeURIComponent(from)}` : ""}`);
  stream = next;
  next.addEventListener("invalidate", (event: MessageEvent<string>) => {
    const body = parse(event.data);
    if (stream !== next || !isInvalidateEvent(body) || !/^\d+$/.test(event.lastEventId)) return;
    const version = BigInt(event.lastEventId);
    if (!cursor || version > BigInt(cursor)) cursor = event.lastEventId;
    for (const entry of listeners) {
      if (entry.snapshot && version <= BigInt(entry.snapshot) || entry.delivered && version <= BigInt(entry.delivered)) continue;
      if (body.resources.some((resource) => entry.keys.some((key) =>
        key.kind === resource.kind && (!key.id || !resource.id || key.id === resource.id),
      ))) {
        entry.delivered = event.lastEventId;
        entry.notify();
      }
    }
  });
  next.addEventListener("reset", (event: MessageEvent<string>) => {
    if (stream !== next || !isResetEvent(parse(event.data))) return;
    disconnect();
    cursor = undefined;
    for (const entry of listeners) {
      delete entry.snapshot;
      delete entry.delivered;
      entry.notify();
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
