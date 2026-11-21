import { queryString, read } from "./client";
import { isPatchSearchResponse } from "./generated/validators.mjs";

const MAX_REPLY_BYTES = 256 * 1024;
const MAX_MATCHES = 24;

export async function readPatchSearch(
  reviewId: string,
  revision: string,
  query: string,
  signal: AbortSignal,
  options?: { readonly cursor?: string; readonly visibleFileId?: string },
) {
  const params = queryString({ revision, query, ...options });
  const response = await read(
    `/reviews/${encodeURIComponent(reviewId)}/changes/search${params}`,
    isPatchSearchResponse,
    signal,
    MAX_REPLY_BYTES,
  );
  const { data } = response;
  const needleBytes = new TextEncoder().encode(query).length;
  if (
    data.reviewId !== reviewId ||
    data.revision !== revision ||
    data.items.length > MAX_MATCHES ||
    (data.coverage === "reading") !== (data.nextCursor !== null) ||
    (data.nextCursor !== null && data.nextCursor === options?.cursor) ||
    data.items.some(
      (match) =>
        match.line < 1 ||
        match.hunk >= match.sourceStart ||
        match.sourceEnd - match.sourceStart !== needleBytes ||
        match.highlightEnd > match.snippet.length ||
        match.highlightEnd <= match.highlightStart ||
        match.snippet.slice(match.highlightStart, match.highlightEnd) !== query,
    )
  ) {
    throw new Error("The patch search response changed or exceeded its bounds.");
  }
  signal.throwIfAborted();
  return response;
}
