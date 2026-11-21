import { useState } from "react";
import { readPatchDiscussions } from "../../../api/changes";
import { lostDiscussionAccess } from "../../../api/discussion-search";
import type { PatchDiscussion } from "../../../api/generated/contracts";
import { useResource } from "../../../hooks/useResource";

const MAX_DISCUSSION_PAGES = 50;
const MAX_DISCUSSION_BYTES = 4 * 1024 * 1024;
const EMPTY_DISCUSSIONS: readonly PatchDiscussion[] = [];

type Page = Awaited<ReturnType<typeof readPatchDiscussions>>;
type Coverage = {
  readonly pages: ReadonlyMap<string | undefined, Page>;
  readonly items: readonly PatchDiscussion[];
  readonly limited: boolean;
};

export function usePatchDiscussions(
  reviewId: string,
  revision: string | undefined,
  generation: number,
) {
  const key = JSON.stringify(["discussions", reviewId, revision, generation]);
  const [progress, setProgress] = useState<{
    readonly key: string;
    readonly data: Coverage | null;
  }>();
  const confirmed = progress?.key === key && progress.data === null;
  const resource = useResource(
    key,
    async (signal) => {
      if (!revision) return { data: null, cursor: "" };
      const items = new Map<string, PatchDiscussion>();
      const pages = new Map<string | undefined, Page>();
      let bytes = 0;
      let next: string | undefined;
      let cursor = "";
      for (let page = 0; page < MAX_DISCUSSION_PAGES; page++) {
        const result = await readPatchDiscussions(reviewId, revision, signal, next, page === 0);
        signal.throwIfAborted();
        cursor = result.cursor;
        const size = 2 * JSON.stringify(result.data.items).length;
        // Retain or drop the whole page. A cursor always follows the full page.
        if (bytes + size > MAX_DISCUSSION_BYTES)
          return { cursor, data: { pages, items: [...items.values()], limited: true } };
        bytes += size;
        pages.set(next, result);
        for (const thread of result.data.items) {
          if (!items.has(thread.discussionId)) items.set(thread.discussionId, thread);
        }
        const following = result.data.nextCursor;
        if (!following || pages.has(following))
          return {
            cursor,
            data: { pages, items: [...items.values()], limited: Boolean(following) },
          };
        if (!confirmed) {
          setProgress({
            key,
            data: { pages: new Map(pages), items: [...items.values()], limited: true },
          });
        }
        next = following;
      }
      return { cursor, data: { pages, items: [...items.values()], limited: true } };
    },
    [{ kind: "discussions", id: reviewId }],
    lostDiscussionAccess,
  );
  // Keep only the key once confirmed, so refreshes need no hidden preview snapshots.
  if (resource.data !== undefined && !confirmed) setProgress({ key, data: null });
  else if (progress && progress.key !== key) setProgress(undefined);
  const coverage =
    resource.data ?? (!resource.error && progress?.key === key ? progress.data : undefined);
  return {
    ...resource,
    items: coverage?.items ?? EMPTY_DISCUSSIONS,
    pages: coverage?.pages,
    limited: coverage?.limited ?? Boolean(revision),
  };
}
