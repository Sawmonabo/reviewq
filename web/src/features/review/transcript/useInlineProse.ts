import { useEffect, useRef, useState } from "react";
import type { HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import { historyContent } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";

const MAX_INLINE_BYTES = 64 * 1024;
const MAX_NEARBY_REFERENCES = 6;
const MAX_REQUESTS = 2;
const MAX_CACHED_REFERENCES = 16;
const empty: ReadonlyMap<string, string | null> = new Map();
const encoder = new TextEncoder();

type Address = { readonly entryId: string; readonly reference: string };
type Cache = {
  readonly scope: string;
  readonly entries: ReadonlyMap<string, string | null>;
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
    function cancel() {
      for (const controller of requests.values()) controller.abort();
      requests.clear();
    }
    window.addEventListener("pagehide", cancel);
    return () => {
      window.removeEventListener("pagehide", cancel);
      cancel();
    };
  }, [scope]);

  useEffect(() => {
    if (!generation) return;
    const addresses = JSON.parse(wanted) as Address[];
    const wantedRefs = new Set(addresses.map(({ reference }) => reference));
    const requests = pending.current;
    for (const [reference, controller] of requests) {
      if (wantedRefs.has(reference)) continue;
      controller.abort();
      requests.delete(reference);
    }

    const known = cache.scope === scope ? cache.entries : empty;
    const timer = window.setTimeout(() => {
      for (const { entryId, reference } of addresses) {
        if (requests.size >= MAX_REQUESTS) break;
        if (known.has(reference) || requests.has(reference)) continue;
        const controller = new AbortController();
        requests.set(reference, controller);
        function save(text: string | null) {
          if (controller.signal.aborted) return;
          requests.delete(reference);
          setCache((current) => {
            const entries = new Map(current.scope === scope ? current.entries : empty);
            entries.set(reference, text);
            while (entries.size > MAX_CACHED_REFERENCES) {
              const oldest = entries.keys().next().value;
              if (oldest === undefined) break;
              entries.delete(oldest);
            }
            return { scope, entries };
          });
        }
        void historyContent(
          { runId, sessionKey },
          entryId,
          generation,
          reference,
          undefined,
          controller.signal,
        ).then(
          (data) => {
            save(
              data.offset === 0 &&
                data.complete &&
                !data.nextCursor &&
                encoder.encode(data.text).length <= MAX_INLINE_BYTES
                ? data.text
                : null,
            );
          },
          () => save(null),
        );
      }
    }, 80);
    return () => window.clearTimeout(timer);
  }, [cache, generation, runId, scope, sessionKey, wanted]);

  return shown.scope === scope ? shown.entries : empty;
}
