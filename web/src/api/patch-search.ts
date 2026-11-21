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
  const literal = new RegExp("^" + query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "$", "iu");
  const encoder = new TextEncoder();
  if (
    data.reviewId !== reviewId ||
    data.revision !== revision ||
    data.items.length > MAX_MATCHES ||
    (data.coverage === "reading") !== (data.nextCursor !== null) ||
    (data.nextCursor !== null && data.nextCursor === options?.cursor) ||
    data.items.some((match) => {
      const matched = match.snippet.slice(match.highlightStart, match.highlightEnd);
      return (
        match.line < 1 ||
        match.hunk >= match.sourceStart ||
        match.sourceEnd - match.sourceStart !== encoder.encode(matched).length ||
        match.highlightEnd > match.snippet.length ||
        match.highlightEnd <= match.highlightStart ||
        !literal.test(matched)
      );
    })
  ) {
    throw new Error("The patch search response changed or exceeded its bounds.");
  }
  signal.throwIfAborted();
  return response;
}
