import { useEffect, useRef, useState } from "react";
import type { Root } from "hast";
import type { HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import { historyContent } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { parseMarkdown } from "../../../components/markdown/parse-markdown";

const MAX_INLINE_BYTES = 64 * 1024;
const MAX_NEARBY_REFERENCES = 6;
const MAX_REQUESTS = 2;
const MAX_CACHED_REFERENCES = 16;
const PREPARE_MIN_BYTES = 8 * 1024;
const PREPARE_IDLE_MS = 20;
const PREPARE_TIMEOUT_MS = 300;
const empty: ReadonlyMap<string, InlineProse | null> = new Map();
const encoder = new TextEncoder();

export type InlineProse =
  | { readonly kind: "source" | "formatting-limit"; readonly text: string }
  | { readonly kind: "prepared"; readonly tree: Root };

type Address = { readonly entryId: string; readonly reference: string };
type Cache = {
  readonly scope: string;
  readonly entries: ReadonlyMap<string, InlineProse | null>;
};

export function inlineProseReference(entry: HistoryEntry, part: HistoryPart) {
  if (
    (part.kind !== "assistant_text" && part.kind !== "user_prompt" && part.kind !== "assignment") ||
    !entry.textBytes ||
    entry.textBytes > MAX_INLINE_BYTES
  )
    return undefined;
  return (
    part.contentRef ??
    (!entry.partsCursor && entry.parts.length === 1 ? entry.contentRef : undefined)
  );
}

export function useInlineProse(
  target: SessionAddress,
  generation: string | undefined,
  nearby: readonly HistoryEntry[],
  hold: boolean,
  openReference: string | undefined,
) {
  const { runId, sessionKey } = target;
  const scope = JSON.stringify([runId, sessionKey, generation]);
  const pending = useRef(new Map<string, AbortController>());
  const nearbyRefs = useRef(new Set<string>());
  const preparing = useRef<AbortController | undefined>(undefined);
  const prepared = useRef<string | undefined>(undefined);
  const requested = useRef(false);
  const [cache, setCache] = useState<Cache>({ scope, entries: empty });
  const [shown, setShown] = useState(cache);
  if (!hold && shown !== cache) setShown(cache);

  const wanted = JSON.stringify(
    nearby
      .flatMap((entry) =>
        entry.parts.flatMap((part) => {
          const reference = inlineProseReference(entry, part);
          return reference && reference !== openReference
            ? [{ entryId: entry.entryId, reference }]
            : [];
        }),
      )
      .slice(0, MAX_NEARBY_REFERENCES),
  );

  useEffect(() => {
    const requests = pending.current;
    requested.current = false;
    prepared.current = undefined;
    const cancel = () => cancelRequests(requests);
    window.addEventListener("pagehide", cancel);
    return () => {
      window.removeEventListener("pagehide", cancel);
      cancel();
    };
  }, [scope]);

  useEffect(() => {
    if (!generation) return;
    const sourceGeneration = generation;
    const addresses = JSON.parse(wanted) as Address[];
    const wantedRefs = new Set(addresses.map(({ reference }) => reference));
    nearbyRefs.current = wantedRefs;
    const requests = pending.current;
    for (const [reference, controller] of requests) {
      if (wantedRefs.has(reference)) continue;
      controller.abort();
      requests.delete(reference);
    }

    const known = cache.scope === scope ? cache.entries : empty;
    // A fresh conversation's closing prose is already at its resting position.
    const debounce = requested.current ? 80 : 0;
    let timer: number | undefined;
    function hasPreparation() {
      return prepared.current !== undefined && nearbyRefs.current.has(prepared.current);
    }
    function fill() {
      if (document.hidden) return;
      for (const { entryId, reference } of addresses) {
        if (requests.size >= MAX_REQUESTS) break;
        if (known.has(reference) || requests.has(reference)) continue;
        const controller = new AbortController();
        requested.current = true;
        requests.set(reference, controller);
        function save(value: InlineProse | null) {
          if (controller.signal.aborted) return;
          requests.delete(reference);
          const replaced = value?.kind === "prepared" ? prepared.current : undefined;
          if (value?.kind === "prepared") prepared.current = reference;
          setCache((current) => {
            const entries = new Map(current.scope === scope ? current.entries : empty);
            if (replaced !== undefined) entries.delete(replaced);
            entries.set(reference, value);
            while (entries.size > MAX_CACHED_REFERENCES) {
              const oldest = [...entries].find(([, item]) => item?.kind !== "prepared")?.[0];
              if (oldest === undefined) break;
              entries.delete(oldest);
            }
            return { scope, entries };
          });
        }
        void historyContent(
          { runId, sessionKey },
          entryId,
          sourceGeneration,
          reference,
          undefined,
          controller.signal,
        ).then(
          (data) => {
            if (controller.signal.aborted) return;
            const bytes = encoder.encode(data.text).length;
            if (
              data.offset !== 0 ||
              !data.complete ||
              data.nextCursor ||
              bytes > MAX_INLINE_BYTES
            ) {
              save(null);
              return;
            }
            const source = { kind: "source", text: data.text } as const;
            if (
              bytes < PREPARE_MIN_BYTES ||
              preparing.current ||
              hasPreparation() ||
              typeof window.requestIdleCallback !== "function"
            ) {
              save(source);
              return;
            }
            // Use the same viewport reads to prepare one expensive body when the browser is idle.
            preparing.current = controller;
            const expires = performance.now() + PREPARE_TIMEOUT_MS;
            let idle = window.requestIdleCallback(prepare, { timeout: PREPARE_TIMEOUT_MS });
            function prepare(deadline: IdleDeadline) {
              if (deadline.timeRemaining() < PREPARE_IDLE_MS && !deadline.didTimeout) {
                idle = window.requestIdleCallback(prepare, {
                  timeout: Math.max(1, expires - performance.now()),
                });
                return;
              }
              controller.signal.removeEventListener("abort", cancelPreparation);
              preparing.current = undefined;
              if (controller.signal.aborted) return;
              if (deadline.didTimeout || hasPreparation()) {
                save(source);
                return;
              }
              const tree = parseMarkdown(source.text);
              save(tree ? { kind: "prepared", tree } : { ...source, kind: "formatting-limit" });
            }
            function cancelPreparation() {
              window.cancelIdleCallback(idle);
              preparing.current = undefined;
            }
            controller.signal.addEventListener("abort", cancelPreparation, { once: true });
          },
          () => save(null),
        );
      }
    }
    function schedule() {
      window.clearTimeout(timer);
      if (document.hidden) cancelRequests(requests);
      else timer = window.setTimeout(fill, debounce);
    }
    schedule();
    document.addEventListener("visibilitychange", schedule);
    return () => {
      document.removeEventListener("visibilitychange", schedule);
      window.clearTimeout(timer);
    };
  }, [cache, generation, runId, scope, sessionKey, wanted]);

  return shown.scope === scope ? shown.entries : empty;
}

function cancelRequests(requests: Map<string, AbortController>) {
  for (const controller of requests.values()) controller.abort();
  requests.clear();
}
