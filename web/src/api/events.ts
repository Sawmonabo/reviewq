import type { ResourceKeyDto } from "./generated/contracts";
import { isInvalidateEvent, isResetEvent } from "./generated/validators.mjs";

type Listener = () => void;
const listeners = new Set<{ keys: readonly ResourceKeyDto[]; notify: Listener }>();
let stream: EventSource | undefined;
let cursor: string | undefined;
let replayFrom: string | undefined;
let reconnect: number | undefined;

export function watchResources(keys: readonly ResourceKeyDto[], notify: Listener): () => void {
  const entry = { keys, notify };
  listeners.add(entry);
  return () => listeners.delete(entry);
}

export function observeSnapshot(value: string) {
  if (!/^\d+$/.test(value)) return;
  if (stream && cursor && BigInt(value) < BigInt(cursor)) {
    if (!replayFrom || BigInt(value) < BigInt(replayFrom)) replayFrom = value;
    if (reconnect === undefined) reconnect = window.setTimeout(replay, 50);
  } else if (!stream) {
    replayFrom = value;
    replay();
  }
}

export function startWorkspaceEvents(): () => void {
  function visibility() {
    if (document.hidden) {
      disconnect();
    } else {
      replayFrom ??= cursor;
      replay();
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
    const body: unknown = parse(event.data);
    if (stream !== next || !isInvalidateEvent(body) || !/^\d+$/.test(event.lastEventId)) return;
    if (!cursor || BigInt(event.lastEventId) > BigInt(cursor)) cursor = event.lastEventId;
    for (const entry of listeners) {
      if (body.resources.some((resource) => entry.keys.some((key) =>
        key.kind === resource.kind && (!key.id || !resource.id || key.id === resource.id),
      ))) entry.notify();
    }
  });
  next.addEventListener("reset", (event: MessageEvent<string>) => {
    if (stream !== next || !isResetEvent(parse(event.data))) return;
    disconnect();
    cursor = undefined;
    for (const entry of listeners) entry.notify();
  });
}

function parse(value: string): unknown {
  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}
