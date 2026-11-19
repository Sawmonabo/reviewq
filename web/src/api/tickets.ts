import { queryString, read } from "./client";
import { isTicketDetailResponse } from "./generated/validators.mjs";

export async function readTicket(key: string, cursor: string | undefined, signal: AbortSignal) {
  const result = await read(`/tickets/${encodeURIComponent(key)}${queryString({ cursor })}`, isTicketDetailResponse, signal);
  if (result.data.key !== key) throw new Error("The returned ticket changed.");
  return result;
}
