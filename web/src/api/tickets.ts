import { ApiResponseError, queryString, read } from "./client";
import { isTicketCommentsResponse, isTicketDetailResponse } from "./generated/validators.mjs";

export type TicketCommentOrder = "newest" | "oldest";

export async function readTicket(key: string, order: TicketCommentOrder, signal: AbortSignal) {
  const result = await read(
    `/tickets/${encodeURIComponent(key)}${queryString({ order })}`,
    isTicketDetailResponse,
    signal,
  );
  if (result.data.key !== key)
    throw new ApiResponseError("conflict", "The returned ticket changed.", false);
  signal.throwIfAborted();
  return result;
}

export async function readTicketComments(
  key: string,
  order: TicketCommentOrder,
  cursor: string | undefined,
  scope: string,
  signal: AbortSignal,
) {
  const result = await read(
    `/tickets/${encodeURIComponent(key)}${queryString({ view: "comments", scope, order, cursor })}`,
    isTicketCommentsResponse,
    signal,
  );
  if (result.data.key !== key || result.data.commentScope !== scope)
    throw new ApiResponseError("conflict", "The returned ticket changed.", false);
  signal.throwIfAborted();
  return result;
}
