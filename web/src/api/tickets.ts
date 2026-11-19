import { queryString, read } from "./client";
import { isTicketDetailResponse } from "./generated/validators.mjs";

export type TicketCommentOrder = "newest" | "oldest";

export async function readTicket(
  key: string,
  order: TicketCommentOrder,
  cursor: string | undefined,
  signal: AbortSignal,
) {
  const result = await read(
    `/tickets/${encodeURIComponent(key)}${queryString({ order, cursor })}`,
    isTicketDetailResponse,
    signal,
  );
  if (result.data.key !== key) throw new Error("The returned ticket changed.");
  return result;
}
