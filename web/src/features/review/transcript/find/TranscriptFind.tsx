import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { ApiResponseError } from "../../../../api/client";
import type {
  HistorySearchHit,
  HistorySearchPage,
} from "../../../../api/generated/contracts/history";
import { historySearch } from "../../../../api/history";
import type { SessionAddress } from "../../../../api/history";
import { Icon } from "../../../../components/Icon";
import { describeError } from "../../../../lib/presentation";
import { clearOccurrence, holdOccurrence, prepareDestination } from "./destination";
import type { PreparedDestination } from "./destination";
import "./find.css";

type Props = {
  readonly target: SessionAddress;
  readonly viewport: RefObject<HTMLDivElement | null>;
  readonly onOpen: () => void;
  readonly onClose: () => void;
  readonly onChoose: (destination: PreparedDestination) => boolean;
  readonly onReveal: () => void;
  readonly onRelease: () => void;
};
type ResultPage = Omit<HistorySearchPage, "hits"> & { readonly hits: readonly HistorySearchHit[] };

export function TranscriptFind({
  target,
  viewport,
  onOpen,
  onClose,
  onChoose,
  onReveal,
  onRelease,
}: Props) {
  const { runId, sessionKey } = target;
  const root = useRef<HTMLDivElement>(null);
  const invoker = useRef<HTMLElement | null>(null);
  const invokedByButton = useRef(false);
  const input = useRef<HTMLInputElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const reads = useRef<AbortController | undefined>(undefined);
  const jumps = useRef<AbortController | undefined>(undefined);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [page, setPage] = useState<ResultPage>();
  const [active, setActive] = useState<HistorySearchHit>();
  const [pending, setPending] = useState(false);
  const [partial, setPartial] = useState(false);
  const [notice, setNotice] = useState<string>();
  const [reload, setReload] = useState(0);

  function show(from: HTMLElement | null) {
    if (open) {
      input.current?.focus({ preventScroll: true });
      return;
    }
    invoker.current = from;
    invokedByButton.current = from === button.current;
    onOpen();
    setOpen(true);
  }

  function close() {
    reads.current?.abort();
    jumps.current?.abort();
    clearOccurrence();
    onRelease();
    onClose();
    setOpen(false);
    setQuery("");
    setPage(undefined);
    setActive(undefined);
    setNotice(undefined);
    setPending(false);
    setPartial(false);
    requestAnimationFrame(() => {
      const target =
        invokedByButton.current && button.current
          ? button.current
          : invoker.current?.isConnected
            ? invoker.current
            : viewport.current;
      target?.focus({ preventScroll: true });
    });
  }

  useEffect(() => {
    if (open) input.current?.focus({ preventScroll: true });
  }, [open]);

  useEffect(() => {
    const host = viewport.current?.closest(".transcript-shell");
    function shortcut(event: Event) {
      if (
        !(event instanceof KeyboardEvent) ||
        event.defaultPrevented ||
        event.key.toLowerCase() !== "f" ||
        !(event.ctrlKey || event.metaKey) ||
        event.altKey
      )
        return;
      const from = event.target instanceof HTMLElement ? event.target : null;
      if (
        event.target instanceof Element &&
        event.target.closest(".mermaid-figure, textarea, [contenteditable=true]")
      )
        return;
      if (!root.current?.contains(from) && !viewport.current?.contains(from)) return;
      event.preventDefault();
      if (open) input.current?.focus({ preventScroll: true });
      else {
        invoker.current = from;
        invokedByButton.current = from === button.current;
        onOpen();
        setOpen(true);
      }
    }
    host?.addEventListener("keydown", shortcut);
    return () => host?.removeEventListener("keydown", shortcut);
  }, [open, viewport, onOpen]);

  useEffect(() => {
    const view = viewport.current;
    function interrupt() {
      jumps.current?.abort();
      onRelease();
    }
    view?.addEventListener("wheel", interrupt, { passive: true });
    view?.addEventListener("touchstart", interrupt, { passive: true });
    view?.addEventListener("pointerdown", interrupt, { passive: true });
    view?.addEventListener("keydown", interrupt);
    return () => {
      view?.removeEventListener("wheel", interrupt);
      view?.removeEventListener("touchstart", interrupt);
      view?.removeEventListener("pointerdown", interrupt);
      view?.removeEventListener("keydown", interrupt);
    };
  }, [viewport, onRelease]);

  useEffect(() => {
    if (!open || !query) return;
    const request = new AbortController();
    reads.current = request;
    const timer = window.setTimeout(() => {
      void readMatches({ runId, sessionKey }, query, false, undefined, request.signal).then(
        (result) => {
          if (request.signal.aborted) return;
          setPage(result);
          if (result.availability === "partial") setPartial(true);
          setPending(false);
        },
        (failure: unknown) => {
          if (request.signal.aborted) return;
          setNotice(describeError(failure));
          setPending(false);
        },
      );
    }, 60);
    return () => {
      window.clearTimeout(timer);
      request.abort();
    };
  }, [runId, sessionKey, open, query, reload]);

  useEffect(
    () => () => {
      reads.current?.abort();
      jumps.current?.abort();
      clearOccurrence();
    },
    [],
  );

  function refine(value: string) {
    reads.current?.abort();
    jumps.current?.abort();
    clearOccurrence();
    onRelease();
    setNotice(undefined);
    setActive(undefined);
    setPage(undefined);
    setPending(Boolean(value));
    setPartial(false);
    setQuery(value);
  }

  async function choose(hit: HistorySearchHit, request: AbortController) {
    const destination = await prepareDestination(target, hit, request.signal);
    request.signal.throwIfAborted();
    if (!onChoose(destination)) {
      setNotice("Clear the active selection before opening another occurrence.");
      return;
    }
    setActive(hit);
    const view = viewport.current;
    if (view)
      holdOccurrence(
        view,
        destination.rowId,
        destination.output.destination,
        request.signal,
        onReveal,
        () => setNotice("The occurrence could not be highlighted."),
      );
  }

  async function select(hit: HistorySearchHit) {
    jumps.current?.abort();
    const request = new AbortController();
    jumps.current = request;
    setNotice(undefined);
    try {
      await choose(hit, request);
    } catch (failure) {
      if (!request.signal.aborted) setNotice(describeError(failure));
    }
  }

  async function step(reverse: boolean) {
    if (!page || !query) return;
    const shown = page.hits;
    const current = active ? shown.findIndex((hit) => hit.hit === active.hit) : -1;
    const next = shown[current + (reverse ? -1 : 1)];
    if (next && (current >= 0 || !reverse)) {
      await select(next);
      return;
    }
    jumps.current?.abort();
    const request = new AbortController();
    jumps.current = request;
    setNotice(undefined);
    try {
      const cursor = active ? (reverse ? active.beforeCursor : active.afterCursor) : undefined;
      let result = await readMatches(target, query, reverse, cursor, request.signal);
      request.signal.throwIfAborted();
      if (result.availability === "partial") setPartial(true);
      if (cursor && !result.hits.length && !result.nextCursor) {
        result = await readMatches(target, query, reverse, undefined, request.signal);
      }
      request.signal.throwIfAborted();
      if (result.availability === "partial") setPartial(true);
      if (result.hits.length) {
        const hit = result.hits[0];
        setPage(reverse ? { ...result, hits: [...result.hits].reverse() } : result);
        if (hit) await choose(hit, request);
      }
    } catch (failure) {
      if (!request.signal.aborted) setNotice(describeError(failure));
    }
  }

  return (
    <div
      ref={root}
      className="transcript-find"
      onKeyDown={(event) => {
        if (event.key !== "Escape" || !open) return;
        event.preventDefault();
        event.stopPropagation();
        close();
      }}
    >
      {!open ? (
        <button
          ref={button}
          type="button"
          className="quiet-button"
          aria-label="Find in conversation"
          onClick={() => show(button.current)}
        >
          <Icon name="search" />
        </button>
      ) : (
        <div className="transcript-find-panel surface-card">
          <div className="transcript-find-controls">
            <input
              ref={input}
              type="search"
              aria-label="Search conversation"
              placeholder="Find in conversation"
              value={query}
              maxLength={256}
              onChange={(event) => refine(event.currentTarget.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  void step(event.shiftKey);
                }
              }}
            />
            <button
              type="button"
              className="quiet-button"
              aria-label="Previous occurrence"
              disabled={!page?.hits.length}
              onClick={() => void step(true)}
            >
              <Icon name="chevronLeft" />
            </button>
            <button
              type="button"
              className="quiet-button"
              aria-label="Next occurrence"
              disabled={!page?.hits.length}
              onClick={() => void step(false)}
            >
              <Icon name="chevronRight" />
            </button>
            <button
              type="button"
              className="quiet-button"
              aria-label="Close conversation Find"
              onClick={close}
            >
              <Icon name="close" />
            </button>
          </div>
          {query && (notice || pending || partial || !page?.hits.length) && (
            <p className="meta transcript-find-status" role="status">
              {notice ??
                (pending
                  ? "Finding matches…"
                  : partial
                    ? "Some conversation text could not be searched."
                    : page?.availability === "unavailable"
                      ? "This conversation cannot be searched."
                      : "No matches.")}
              {notice && (
                <button
                  type="button"
                  className="quiet-button"
                  onClick={() => {
                    setNotice(undefined);
                    setPending(true);
                    setReload((value) => value + 1);
                  }}
                >
                  Retry Find
                </button>
              )}
            </p>
          )}
          {!active && page?.hits.length ? (
            <div className="transcript-find-results" aria-label="Conversation matches">
              {page.hits.map((hit) => (
                <button
                  type="button"
                  className="quiet-button"
                  key={hit.hit}
                  onClick={() => void select(hit)}
                >
                  {hit.before}
                  <mark>{hit.matched}</mark>
                  {hit.after}
                </button>
              ))}
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}

async function readMatches(
  target: SessionAddress,
  query: string,
  reverse: boolean,
  start: string | undefined,
  signal: AbortSignal,
) {
  let cursor = start;
  let partial = false;
  for (;;) {
    const page = await searchWithRetry(
      target,
      { q: query, reverse, ...(cursor ? { cursor } : {}) },
      signal,
    );
    partial ||= page.availability === "partial";
    if (!page.nextCursor || page.hits.length)
      return partial ? { ...page, availability: "partial" as const } : page;
    if (page.nextCursor === cursor) throw new Error("Conversation Find received a repeated page.");
    cursor = page.nextCursor;
  }
}

async function searchWithRetry(
  target: SessionAddress,
  parameters: Parameters<typeof historySearch>[1],
  signal: AbortSignal,
) {
  for (const delay of [70, 150, 300, 600, 1200]) {
    try {
      return await historySearch(target, parameters, signal);
    } catch (error) {
      if (!(error instanceof ApiResponseError) || !error.retryable || error.code !== "busy")
        throw error;
      signal.throwIfAborted();
      await new Promise<void>((resolve, reject) => {
        const timer = window.setTimeout(() => {
          signal.removeEventListener("abort", cancel);
          resolve();
        }, delay);
        function cancel() {
          window.clearTimeout(timer);
          reject(new DOMException("Aborted", "AbortError"));
        }
        signal.addEventListener("abort", cancel, { once: true });
      });
    }
  }
  return historySearch(target, parameters, signal);
}
