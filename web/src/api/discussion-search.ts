import { ApiResponseError, queryString, read } from "./client";
import {
  isDiscussionDestinationResponse,
  isDiscussionSearchResponse,
} from "./generated/validators.mjs";

function path(reviewId: string) {
  return `/reviews/${encodeURIComponent(reviewId)}/changes/notes`;
}

export async function readDiscussionSearch(
  reviewId: string,
  revision: string,
  query: string,
  cursor: string | undefined,
  signal: AbortSignal,
) {
  const result = await read(
    `${path(reviewId)}${queryString({ revision, query, cursor })}`,
    isDiscussionSearchResponse,
    signal,
  );
  checkScope(result.data, reviewId, revision);
  signal.throwIfAborted();
  return result;
}

export async function readDiscussionDestination(
  reviewId: string,
  revision: string,
  hit: string,
  signal: AbortSignal,
  recheck?: boolean,
) {
  const result = await read(
    `${path(reviewId)}/destination${queryString({ revision, hit, recheck })}`,
    isDiscussionDestinationResponse,
    signal,
  );
  checkScope(result.data, reviewId, revision);
  if (!result.data.thread.notes.some((note) => note.noteId === result.data.noteId)) stale();
  signal.throwIfAborted();
  return result;
}

function checkScope(
  value: { readonly reviewId: string; readonly revision: string },
  reviewId: string,
  revision: string,
) {
  if (value.reviewId !== reviewId || value.revision !== revision) stale();
}

function stale(): never {
  throw new ApiResponseError("cursor_expired", "The discussion changed. Search again.", false);
}

export function lostDiscussionAccess(error: unknown) {
  return (
    error instanceof ApiResponseError &&
    ["forbidden", "unauthorized", "not_found"].includes(error.code)
  );
}
