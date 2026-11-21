import { useState } from "react";
import { readPatchSearch } from "../../../api/patch-search";
import { useResource } from "../../../hooks/useResource";

export function usePatchSearch(
  reviewId: string,
  revision: string,
  query: string,
  visibleFileId: string | undefined,
) {
  const [position, setPosition] = useState<{ readonly query: string; readonly cursor: string }>();
  const cursor = position?.query === query ? position.cursor : undefined;
  const resource = useResource(
    JSON.stringify(["patch-search", reviewId, revision, query, visibleFileId, cursor]),
    async (signal) => {
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
      return page;
    },
  );
  return {
    ...resource,
    reset: () => setPosition(undefined),
    more: () => {
      if (resource.data?.nextCursor) setPosition({ query, cursor: resource.data.nextCursor });
    },
  };
}
