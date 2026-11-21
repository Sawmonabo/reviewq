import { useEffect, useState } from "react";
import { readDiscussionSearch } from "../../../api/discussion-search";
import { observeSnapshot } from "../../../api/events";
import type { DiscussionSearch } from "../../../api/generated/contracts";
import { describeError } from "../../../lib/presentation";

type Trail = { readonly scope: string; readonly cursors: readonly string[] };
type Result = { readonly key: string; readonly data?: DiscussionSearch; readonly error?: string };

export function useDiscussionSearch(
  reviewId: string,
  revision: string,
  query: string,
  generation: number,
) {
  const scope = JSON.stringify([reviewId, revision, query, generation]);
  const [position, setPosition] = useState<Trail>();
  const [retry, setRetry] = useState(0);
  const [result, setResult] = useState<Result>();
  const trail = position?.scope === scope ? position.cursors : [];
  const cursor = trail.at(-1);
  const key = JSON.stringify([scope, cursor, retry]);
  if (result && result.key !== key) setResult(undefined);

  useEffect(() => {
    if (!query) return;
    const controller = new AbortController();
    const { signal } = controller;
    async function search() {
      let response = await readDiscussionSearch(reviewId, revision, query, cursor, signal);
      for (
        let pages = 0;
        pages < 5 && !response.data.items.length && response.data.nextCursor;
        pages++
      ) {
        response = await readDiscussionSearch(
          reviewId,
          revision,
          query,
          response.data.nextCursor,
          signal,
        );
      }
      signal.throwIfAborted();
      setResult({ key, data: response.data });
      observeSnapshot(response.cursor);
    }
    void search().catch((error: unknown) => {
      if (!signal.aborted) setResult({ key, error: describeError(error) });
    });
    return () => controller.abort();
  }, [cursor, key, query, reviewId, revision]);

  const current = result?.key === key ? result : undefined;
  return {
    data: current?.data,
    error: current?.error,
    pending: Boolean(query && !current),
    retry: () => {
      setPosition(undefined);
      setRetry((value) => value + 1);
    },
    hasPrevious: trail.length > 0,
    previous: () => setPosition({ scope, cursors: trail.slice(0, -1) }),
    more: () => {
      if (current?.data?.nextCursor)
        setPosition({ scope, cursors: [...trail.slice(-31), current.data.nextCursor] });
    },
  };
}
