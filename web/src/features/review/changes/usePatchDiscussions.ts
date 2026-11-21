import { useState } from "react";
import { readPatchDiscussions } from "../../../api/changes";
import type { PatchDiscussion } from "../../../api/generated/contracts";
import { useResource } from "../../../hooks/useResource";

const MAX_DISCUSSION_PAGES = 50;
const MAX_DISCUSSION_BYTES = 4 * 1024 * 1024;
const EMPTY_DISCUSSIONS: readonly PatchDiscussion[] = [];

export function usePatchDiscussions(reviewId: string, revision: string | undefined) {
  const key = JSON.stringify(["discussions", reviewId, revision]);
  const [progress, setProgress] = useState<{
    readonly key: string;
    readonly items: readonly PatchDiscussion[];
  }>();
  const resource = useResource(
    key,
    async (signal) => {
      if (!revision) return { data: null, cursor: "" };
      const items = new Map<string, PatchDiscussion>();
      let bytes = 0;
      let next: string | undefined;
      let cursor = "";
      let limited = false;
      for (let page = 0; page < MAX_DISCUSSION_PAGES; page++) {
        const result = await readPatchDiscussions(reviewId, revision, signal, next);
        signal.throwIfAborted();
        cursor = result.cursor;
        for (const thread of result.data.items) {
          if (items.has(thread.discussionId)) continue;
          const size = 2 * JSON.stringify(thread).length;
          if (bytes + size > MAX_DISCUSSION_BYTES) {
            limited = true;
            break;
          }
          bytes += size;
          items.set(thread.discussionId, thread);
        }
        if (limited || !result.data.nextCursor || result.data.nextCursor === next)
          return {
            cursor,
            data: {
              items: [...items.values()],
              limited: limited || Boolean(result.data.nextCursor),
            },
          };
        setProgress({ key, items: [...items.values()] });
        next = result.data.nextCursor;
      }
      return { cursor, data: { items: [...items.values()], limited: true } };
    },
    [{ kind: "discussions", id: reviewId }],
  );
  const items =
    resource.data?.items ?? (progress?.key === key ? progress.items : EMPTY_DISCUSSIONS);
  return { ...resource, items, limited: resource.data?.limited ?? Boolean(revision) };
}
