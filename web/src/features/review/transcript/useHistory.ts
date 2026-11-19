import { useEffect, useEffectEvent, useRef, useState } from "react";
import { observeSnapshot } from "../../../api/events";
import type { HistoryPage } from "../../../api/generated/contracts";
import { historyPage, nativeHistoryUrl } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { describeError } from "../../../lib/presentation";
import { PAGE_LIMIT, readBookmark } from "./history-window";

type ReadPosition = "older" | "newer" | "latest" | "around";
type Anchor = { readonly entryId: string; readonly locator?: string };
type Delta = {
  readonly runId: string;
  readonly sessionKey: string;
  readonly generation: string;
  readonly turnId: string;
  readonly entryId: string;
  readonly block: number;
  readonly offset: number;
  readonly text: string;
  readonly provisional: boolean;
};
type Provisional = {
  readonly entryId: string;
  readonly turnId: string;
  readonly text: string;
  readonly bytes: number;
};
const utf8 = new TextEncoder();

export function useHistory(
  target: SessionAddress,
  bookmark: string,
  following: boolean,
  pinned: ReadonlySet<string>,
  beforeUpdate: (position: ReadPosition, anchor?: Anchor) => void,
) {
  const { runId, sessionKey } = target;
  const [pages, setPages] = useState<readonly HistoryPage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [online, setOnline] = useState(true);
  const [availability, setAvailability] = useState<string>();
  const [updated, setUpdated] = useState<number>();
  const [waiting, setWaiting] = useState(false);
  const [live, setLive] = useState<Provisional>();
  const active = useRef<AbortController | undefined>(undefined);
  const pending = useRef(false);
  const resetTo = useRef<Anchor | undefined>(undefined);
  const first = pages[0];
  const last = pages.at(-1);
  const generation = last?.generation;

  async function load(position: ReadPosition, wanted?: Anchor, background = false) {
    if (position === "latest" && !background) resetTo.current = undefined;
    const requestedJump = position === "around" && wanted && !background;
    if (active.current && !requestedJump) {
      if (background) pending.current = true;
      return;
    }
    const cursor =
      position === "older"
        ? first?.nextCursor
        : position === "newer"
          ? last?.previousCursor
          : undefined;
    if ((position === "older" || position === "newer") && !cursor) return;
    const adjacent = position === "older" || position === "newer";
    const evict = position === "older" ? last : first;
    if (
      pinned.size &&
      (!adjacent ||
        (pages.length >= PAGE_LIMIT && evict?.items.some((item) => pinned.has(item.entryId))))
    ) {
      if (position === "around") resetTo.current = wanted;
      pending.current = true;
      setWaiting(true);
      return;
    }
    active.current?.abort();
    if (requestedJump) resetTo.current = undefined;
    beforeUpdate(position, wanted);
    const controller = new AbortController();
    active.current = controller;
    pending.current = false;
    setLoading(true);
    try {
      const response = await historyPage(
        target,
        cursor
          ? { cursor }
          : wanted
            ? { aroundId: wanted.entryId, ...(wanted.locator ? { locator: wanted.locator } : {}) }
            : {},
        controller.signal,
      );
      if (controller.signal.aborted) return;
      const { data, cursor: eventCursor } = response;
      observeSnapshot(eventCursor);
      setAvailability(data.availability);
      setUpdated(Date.now());
      setError(undefined);
      setWaiting(pending.current);
      if (data.reset || !adjacent || data.generation !== generation) {
        setPages([data]);
        setLive(undefined);
      } else if (position === "older") {
        setPages((items) => [data, ...items].slice(0, PAGE_LIMIT));
      } else {
        setPages((items) => [...items, data].slice(-PAGE_LIMIT));
        if (data.items.some((entry) => entry.entryId === live?.entryId)) setLive(undefined);
      }
    } catch (failure) {
      if (!controller.signal.aborted) setError(describeError(failure));
    } finally {
      if (active.current === controller) {
        active.current = undefined;
        setLoading(false);
      }
    }
  }

  const initialize = useEffectEvent(() => {
    const saved = readBookmark(bookmark);
    if (saved && !saved.following) void load("around", { entryId: saved.entryId });
    else void load("latest");
  });

  const notice = useEffectEvent((kind: string, value: Record<string, unknown>) => {
    if (kind === "delta") {
      if (
        !isDelta(value) ||
        value.generation !== generation ||
        !following ||
        pinned.size ||
        value.block !== 0
      ) {
        if (!following || pinned.size) {
          pending.current = true;
          setWaiting(true);
        }
        return;
      }
      const bytes = utf8.encode(value.text).length;
      if (bytes > 65_536 || value.offset + bytes > 65_536) return;
      setLive((current) => {
        if (!current || current.entryId !== value.entryId) {
          return value.offset === 0
            ? { entryId: value.entryId, turnId: value.turnId, text: value.text, bytes }
            : current;
        }
        return current.bytes === value.offset
          ? { ...current, text: current.text + value.text, bytes: current.bytes + bytes }
          : current;
      });
      return;
    }
    if (typeof value.availability === "string") setAvailability(value.availability);
    if (!following || pinned.size) {
      pending.current = true;
      setWaiting(true);
      return;
    }
    void load(last?.previousCursor && kind !== "reset" ? "newer" : "latest", undefined, true);
  });

  const catchup = useEffectEvent(() => {
    if (pinned.size || !waiting || loading) return;
    const wanted = resetTo.current;
    if (!wanted && !following) return;
    resetTo.current = undefined;
    void load(wanted ? "around" : "latest", wanted, !wanted);
  });

  useEffect(() => {
    // Establish the stream baseline before taking the first page, so its initial
    // availability notice and the first read observe the same starting point.
    let initialized = false;
    function firstPage() {
      if (initialized) return;
      initialized = true;
      window.clearTimeout(firstRead);
      initialize();
    }
    const firstRead = window.setTimeout(firstPage, 50);
    let stream: EventSource | undefined;
    let retry: number | undefined;
    function close() {
      stream?.close();
      stream = undefined;
      if (retry !== undefined) window.clearTimeout(retry);
    }
    function open() {
      if (document.hidden) {
        close();
        return;
      }
      if (stream) return;
      const next = new EventSource(nativeHistoryUrl({ runId, sessionKey }));
      stream = next;
      next.onopen = () => {
        if (stream === next) setOnline(true);
      };
      next.onerror = () => {
        if (stream !== next) return;
        if (retry !== undefined) window.clearTimeout(retry);
        retry = window.setTimeout(() => setOnline(false), 2600);
      };
      for (const kind of ["changed", "reset", "availability", "delta"]) {
        next.addEventListener(kind, (event: MessageEvent<string>) => {
          if (stream !== next) return;
          const value = parseEvent(event.data);
          if (value?.runId !== runId || value.sessionKey !== sessionKey) return;
          if (!initialized && kind !== "delta") {
            if (typeof value.availability === "string") setAvailability(value.availability);
            firstPage();
          } else {
            notice(kind, value);
          }
        });
      }
    }
    open();
    document.addEventListener("visibilitychange", open);
    window.addEventListener("pagehide", close);
    return () => {
      window.clearTimeout(firstRead);
      const controller = active.current;
      active.current = undefined;
      controller?.abort();
      close();
      document.removeEventListener("visibilitychange", open);
      window.removeEventListener("pagehide", close);
    };
  }, [runId, sessionKey, bookmark]);

  useEffect(() => {
    if (!waiting || loading || pinned.size) return;
    const frame = requestAnimationFrame(() => catchup());
    return () => cancelAnimationFrame(frame);
  }, [following, loading, pinned.size, waiting]);

  return {
    pages,
    older: Boolean(first?.nextCursor),
    newer: Boolean(last?.previousCursor),
    waiting,
    loading,
    error,
    online,
    availability,
    updated,
    live,
    generation,
    load,
  };
}

function isDelta(value: Record<string, unknown>): value is Record<string, unknown> & Delta {
  return (
    typeof value.generation === "string" &&
    typeof value.turnId === "string" &&
    typeof value.entryId === "string" &&
    typeof value.block === "number" &&
    typeof value.offset === "number" &&
    Number.isSafeInteger(value.offset) &&
    value.offset >= 0 &&
    typeof value.text === "string" &&
    value.provisional === true
  );
}

function parseEvent(source: string): Record<string, unknown> | undefined {
  if (source.length > 150_000) return;
  try {
    const value: unknown = JSON.parse(source);
    return typeof value === "object" && value !== null
      ? (value as Record<string, unknown>)
      : undefined;
  } catch {
    return undefined;
  }
}
