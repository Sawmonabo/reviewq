import { useState } from "react";
import type { PatchSearch } from "../../../api/generated/contracts";
import { readPatchSearch } from "../../../api/patch-search";
import { useResource } from "../../../hooks/useResource";

type SearchPosition = { readonly key: string; readonly trail: readonly string[] };

export function usePatchSearch(
  reviewId: string,
  revision: string,
  query: string,
  visibleFileId: string | undefined,
) {
  const key = JSON.stringify([reviewId, revision, query, visibleFileId]);
  const [position, setPosition] = useState<SearchPosition>();
  const [lastResults, setLastResults] = useState<{
    readonly key: string;
    readonly data: PatchSearch;
  }>();
  const trail = position?.key === key ? position.trail : [];
  const cursor = trail.at(-1);
  const resource = useResource(JSON.stringify(["patch-search", key, cursor]), async (signal) => {
    if (!query || new TextEncoder().encode(query).length > 256) return { cursor: "", data: null };
    function read(next: string | undefined) {
      return readPatchSearch(reviewId, revision, query, signal, {
        ...(next && { cursor: next }),
        ...(visibleFileId && { visibleFileId }),
      });
    }
    let page = await read(cursor);
    // A bounded server scan may report progress without any results in this page.
    for (
      let scanned = 0;
      scanned < 4 && !page.data.items.length && page.data.nextCursor;
      scanned++
    ) {
      signal.throwIfAborted();
      page = await read(page.data.nextCursor);
    }
    signal.throwIfAborted();
    if (page.data.items.length) setLastResults({ key, data: page.data });
    return page;
  });
  const known = lastResults?.key === key ? lastResults.data : undefined;
  const data = resource.data;
  return {
    ...resource,
    items: data?.items.length ? data.items : (known?.items ?? []),
    exhausted: Boolean(known && data && !data.nextCursor && !data.items.length),
    hasPrevious: trail.length > 0,
    previous: () => setPosition({ key, trail: trail.slice(0, -1) }),
    reset: () => {
      setPosition(undefined);
      setLastResults(undefined);
    },
    more: () => {
      if (data?.nextCursor) setPosition({ key, trail: [...trail, data.nextCursor] });
    },
  };
}
