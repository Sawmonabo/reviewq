import { useEffect, useRef, useState } from "react";
import { PATCH_FILES_PER_PAGE, readPatchFiles } from "../../../api/changes";
import type { PatchIndex } from "../../../api/changes";
import { observeSnapshot } from "../../../api/events";
import type { PatchPath } from "../../../api/generated/contracts";

const RETAINED_PAGES = 5;

type Lookup =
  | { readonly page: number; readonly cursor: string }
  | { readonly path: PatchPath }
  | { readonly fileId: string };

export function usePatchPages(reviewId: string, initial: PatchIndex, pinned: readonly number[]) {
  const pinnedPages = useRef(new Set<number>());
  useEffect(() => {
    pinnedPages.current = new Set(pinned.map(pageAt));
  }, [pinned]);
  const [pages, setPages] = useState(() => new Map([[initial.pageNumber, initial]]));
  const [tail, setTail] = useState(() => pageBoundary(initial));
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);
  const request = useRef<
    { readonly controller: AbortController; readonly manual: boolean } | undefined
  >(undefined);
  const cursors = useRef(pageCursors(initial));
  useEffect(() => () => request.current?.controller.abort(), []);

  async function load(lookup: Lookup, manual: boolean) {
    if (!manual && request.current) return;
    if ("page" in lookup) {
      const known = pages.get(lookup.page);
      if (known) return known;
    }
    request.current?.controller.abort();
    const controller = new AbortController();
    request.current = { controller, manual };
    setPending(true);
    setError(undefined);
    try {
      const { cursor, data } = await readPatchFiles(reviewId, controller.signal, {
        revision: initial.revision,
        ...lookup,
        ...("page" in lookup && {
          previewEdge: pages.has(lookup.page - 1) ? ("start" as const) : ("end" as const),
        }),
      });
      controller.signal.throwIfAborted();
      if (
        data.headSha !== initial.headSha ||
        data.baseSha !== initial.baseSha ||
        data.startSha !== initial.startSha ||
        data.settingsRevision !== initial.settingsRevision ||
        ("page" in lookup && data.pageNumber !== lookup.page)
      )
        throw new Error("The displayed file index changed.");
      for (const [page, token] of pageCursors(data)) cursors.current.set(page, token);
      if ("page" in lookup) cursors.current.set(lookup.page, lookup.cursor);
      observeSnapshot(cursor);
      setPages((previous) => {
        const retained = new Map(previous);
        retained.set(data.pageNumber, data);
        if (retained.size > RETAINED_PAGES) {
          const distant = [...retained.keys()]
            .filter((page) => !pinnedPages.current.has(page))
            .sort((a, b) => Math.abs(b - data.pageNumber) - Math.abs(a - data.pageNumber));
          for (const page of distant.slice(0, retained.size - RETAINED_PAGES))
            retained.delete(page);
        }
        return retained;
      });
      setTail((previous) =>
        data.pageNumber >= previous.pageNumber ? pageBoundary(data) : previous,
      );
      return data;
    } catch (failure) {
      if (!controller.signal.aborted)
        setError(
          failure instanceof Error ? failure.message : "ReviewQ could not load changed files.",
        );
    } finally {
      if (!controller.signal.aborted) {
        request.current = undefined;
        setPending(false);
      }
    }
  }

  function lookupPage(page: number) {
    if (page < 1 || pages.has(page)) return;
    const exact = cursors.current.get(page);
    if (exact) return { page, cursor: exact };
    // After a direct jump the gap contains undiscovered REST pages. Travel from
    // the nearest signed boundary until the requested page has its own cursor.
    let closest: Extract<Lookup, { page: number }> | undefined;
    for (const [number, cursor] of cursors.current) {
      if (pages.has(number)) continue;
      if (!closest || Math.abs(number - page) < Math.abs(closest.page - page))
        closest = { page: number, cursor };
    }
    return closest;
  }

  function cancelManual() {
    if (!request.current?.manual) return;
    request.current.controller.abort();
    request.current = undefined;
    setPending(false);
  }

  return {
    pages,
    tail,
    pending,
    error,
    load,
    lookupPage,
    cancelManual,
    retry: () => setError(undefined),
  };
}

function pageBoundary(index: PatchIndex) {
  return { pageNumber: index.pageNumber, length: index.items.length, nextCursor: index.nextCursor };
}

function pageCursors(index: PatchIndex) {
  const cursors = new Map<number, string>();
  if (index.previousCursor) cursors.set(index.pageNumber - 1, index.previousCursor);
  if (index.nextCursor) cursors.set(index.pageNumber + 1, index.nextCursor);
  return cursors;
}

export function pageStart(page: number) {
  return (page - 1) * PATCH_FILES_PER_PAGE;
}

export function pageAt(index: number) {
  return Math.floor(index / PATCH_FILES_PER_PAGE) + 1;
}
