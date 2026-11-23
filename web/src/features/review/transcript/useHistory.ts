import { useEffect, useEffectEvent, useLayoutEffect, useRef, useState } from "react";
import { observeSnapshot, watchResources } from "../../../api/events";
import type { HistoryPage, HistoryPageResponse } from "../../../api/generated/contracts";
import { historyPage, nativeHistoryUrl } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { isWorkspaceConnectionFailure } from "../../../hooks/useWorkspaceResource";
import { describeError } from "../../../lib/presentation";
import { deliveryRowKey, historyKey, PAGE_LIMIT, readBookmark } from "./history-window";

type ReadPosition = "older" | "newer" | "latest" | "around";
type ReadMode = "manual" | "refresh" | "prefill";
type Anchor = { readonly entryId: string; readonly locator?: string; readonly rowId?: string };
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
  focusedDiagram: string | undefined,
  beforeUpdate: (position: ReadPosition, anchor: Anchor | undefined, extend: boolean) => void,
) {
  const { runId, sessionKey } = target;
  const [pages, setPages] = useState<readonly HistoryPage[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<{ message: string; connection: boolean }>();
  const [online, setOnline] = useState(true);
  const [availability, setAvailability] = useState<string>();
  const [updated, setUpdated] = useState<number>();
  const [waiting, setWaiting] = useState(false);
  const [live, setLive] = useState<Provisional>();
  const active = useRef<{ controller: AbortController; position: ReadPosition } | undefined>(
    undefined,
  );
  const currentView = useRef({ beforeUpdate, pinned, focusedDiagram });
  useLayoutEffect(() => {
    currentView.current = { beforeUpdate, pinned, focusedDiagram };
  }, [beforeUpdate, pinned, focusedDiagram]);
  const pending = useRef(false);
  const reopen = useRef<(() => void) | undefined>(undefined);
  const resetWhileFocused = useRef(false);
  const resetTo = useRef<Anchor | undefined>(undefined);
  const first = pages[0];
  const last = pages.at(-1);
  const generation = last?.generation;

  function cancelOpposite(up: boolean) {
    const request = active.current;
    if (request?.position === (up ? "newer" : "older")) {
      request.controller.abort();
      active.current = undefined;
      setLoading(false);
    }
  }

  function openPrepared(response: HistoryPageResponse, wanted: Anchor) {
    if (currentView.current.pinned.size || response.data.reset) return false;
    active.current?.controller.abort();
    active.current = undefined;
    pending.current = false;
    resetTo.current = undefined;
    currentView.current.beforeUpdate("around", wanted, false);
    observeSnapshot(response.cursor);
    setAvailability(response.data.availability);
    setUpdated(Date.now());
    setLoading(false);
    setWaiting(false);
    setError(undefined);
    setPages([response.data]);
    setLive(undefined);
    return true;
  }

  function sameLiveDiagram(entryId: string | undefined) {
    const { focusedDiagram } = currentView.current;
    return entryId !== undefined && focusedDiagram === entryId;
  }

  async function load(position: ReadPosition, wanted?: Anchor, mode: ReadMode = "manual") {
    const background = mode !== "manual";
    if (position === "latest" && !background) resetTo.current = undefined;
    const requestedJump =
      !background && (position === "latest" || (position === "around" && wanted !== undefined));
    if (active.current && !requestedJump) {
      if (mode === "refresh") pending.current = true;
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
    function held(replace: boolean, refreshed?: HistoryPage) {
      const protectedIds = currentView.current.pinned;
      if (!protectedIds.size) return false;
      if (background && sameLiveDiagram(live?.entryId) && !resetWhileFocused.current) {
        if (!refreshed || (!refreshed.reset && refreshed.generation === generation)) return false;
        resetWhileFocused.current = true;
      }
      if (
        !replace &&
        (pages.length < PAGE_LIMIT ||
          !evict?.items.some(
            (item) =>
              protectedIds.has(historyKey(item)) ||
              (item.messageId && protectedIds.has(deliveryRowKey(item.messageId))),
          ))
      )
        return false;
      if (position === "around") resetTo.current = wanted;
      pending.current = true;
      setWaiting(true);
      return true;
    }
    if (held(!adjacent)) return;
    resetWhileFocused.current = false;
    active.current?.controller.abort();
    if (requestedJump) resetTo.current = undefined;
    const controller = new AbortController();
    active.current = { controller, position };
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
      const replacing = data.reset || !adjacent || data.generation !== generation;
      if (mode === "prefill" && (replacing || currentView.current.pinned.size)) return;
      if (held(replacing, data)) return;
      const previousHead = last?.items[0];
      const refreshedHead = data.items[0];
      // Earlier pages still reach the same boundary when a live refresh keeps its head entry.
      const keepEarlier =
        mode === "refresh" &&
        position === "latest" &&
        pages.length > 1 &&
        !data.reset &&
        data.generation === generation &&
        data.nextCursor !== null &&
        previousHead !== undefined &&
        refreshedHead !== undefined &&
        historyKey(previousHead) === historyKey(refreshedHead);
      currentView.current.beforeUpdate(position, wanted, !replacing || keepEarlier);
      observeSnapshot(eventCursor);
      setAvailability(data.availability);
      setUpdated(Date.now());
      setError(undefined);
      setWaiting(pending.current);
      if (replacing) {
        setPages(keepEarlier ? [...pages.slice(0, -1), data] : [data]);
        setLive((current) =>
          background &&
          current &&
          sameLiveDiagram(current.entryId) &&
          !data.reset &&
          data.generation === generation &&
          !data.items.some((item) => item.entryId === current.entryId)
            ? current
            : undefined,
        );
      } else if (position === "older") {
        setPages((items) => [data, ...items].slice(0, PAGE_LIMIT));
      } else {
        setPages((items) => [...items, data].slice(-PAGE_LIMIT));
        if (data.items.some((entry) => entry.entryId === live?.entryId)) setLive(undefined);
      }
    } catch (failure) {
      if (!controller.signal.aborted && mode !== "prefill") {
        setError({
          message: describeError(failure),
          connection: isWorkspaceConnectionFailure(failure),
        });
      }
    } finally {
      if (active.current?.controller === controller) {
        active.current = undefined;
        if (mode === "prefill" && pending.current) setWaiting(true);
        setLoading(false);
      }
    }
  }

  const initialize = useEffectEvent(() => {
    if (active.current) return;
    const saved = readBookmark(bookmark);
    if (saved && !saved.following)
      void load("around", {
        entryId: saved.entryId,
        ...(saved.rowId ? { rowId: saved.rowId } : {}),
      });
    else void load("latest");
  });

  const notice = useEffectEvent((kind: string, value: Record<string, unknown>) => {
    if (kind === "delta") {
      if (!isDelta(value) || value.generation !== generation || value.block !== 0) return;
      if (!sameLiveDiagram(value.entryId) && (!following || pinned.size)) {
        pending.current = true;
        setWaiting(true);
        return;
      }
      const bytes = utf8.encode(value.text).length;
      if (bytes > 65_536 || value.offset + bytes > 65_536) return;
      setLive((current) => {
        if (!current || current.entryId !== value.entryId || current.turnId !== value.turnId) {
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
    if (
      (resetWhileFocused.current || !sameLiveDiagram(live?.entryId)) &&
      (!following || pinned.size)
    ) {
      pending.current = true;
      setWaiting(true);
      return;
    }
    void load(last?.previousCursor && kind !== "reset" ? "newer" : "latest", undefined, "refresh");
  });

  const resumeAfterInterruption = useEffectEvent(() => {
    if (last) notice("reset", {});
    else initialize();
  });

  const catchup = useEffectEvent(() => {
    const diagram = !resetWhileFocused.current && sameLiveDiagram(live?.entryId);
    if ((!diagram && pinned.size) || !waiting || loading) return;
    const wanted = resetTo.current;
    if (!wanted && !following && !diagram) return;
    resetTo.current = undefined;
    void load(wanted ? "around" : "latest", wanted, wanted ? "manual" : "refresh");
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
    let pauseTimer: number | undefined;
    let interrupted = false;
    function clearPause() {
      if (pauseTimer !== undefined) window.clearTimeout(pauseTimer);
      pauseTimer = undefined;
    }
    function close() {
      stream?.close();
      stream = undefined;
      clearPause();
    }
    function open() {
      if (document.hidden) {
        close();
        return;
      }
      if (stream && stream.readyState !== EventSource.CLOSED) return;
      close();
      const next = new EventSource(nativeHistoryUrl({ runId, sessionKey }));
      stream = next;
      next.onopen = () => {
        if (stream !== next) return;
        clearPause();
        setOnline(true);
        if (interrupted && initialized) resumeAfterInterruption();
        interrupted = false;
      };
      next.onerror = () => {
        if (stream !== next) return;
        interrupted = true;
        if (pauseTimer === undefined) {
          pauseTimer = window.setTimeout(() => {
            if (stream === next) setOnline(false);
          }, 2600);
        }
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
    reopen.current = () => {
      close();
      open();
    };
    open();
    const subscription = watchResources([{ kind: "history", id: sessionKey }], (reason) => {
      if (reason === "visible" || reason === "reset") open();
    });
    document.addEventListener("visibilitychange", open);
    window.addEventListener("online", open);
    window.addEventListener("pagehide", close);
    window.addEventListener("pageshow", open);
    return () => {
      window.clearTimeout(firstRead);
      const controller = active.current?.controller;
      active.current = undefined;
      controller?.abort();
      close();
      reopen.current = undefined;
      subscription.close();
      document.removeEventListener("visibilitychange", open);
      window.removeEventListener("online", open);
      window.removeEventListener("pagehide", close);
      window.removeEventListener("pageshow", open);
    };
  }, [runId, sessionKey, bookmark]);

  useEffect(() => {
    if (!waiting || loading) return;
    const frame = requestAnimationFrame(() => catchup());
    return () => cancelAnimationFrame(frame);
  }, [following, loading, pinned.size, focusedDiagram, waiting]);

  return {
    pages,
    older: Boolean(first?.nextCursor),
    newer: Boolean(last?.previousCursor),
    waiting,
    loading,
    error: error?.message,
    connectionError: error?.connection ?? false,
    online,
    availability,
    updated,
    live,
    generation,
    cancelOpposite,
    reconnect: () => reopen.current?.(),
    load,
    openPrepared,
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
