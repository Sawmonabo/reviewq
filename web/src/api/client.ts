import type {
  QueuePageResponse,
  ReviewOverviewResponse,
  ReviewSelectionResponse,
  RunDetailResponse,
} from "./generated/contracts";
import {
  isFailure,
  isQueuePageResponse,
  isReviewOverviewResponse,
  isReviewSelectionResponse,
  isRunDetailResponse,
} from "./generated/validators.mjs";

let session: Promise<string> | undefined;

export async function readQueue(signal: AbortSignal): Promise<QueuePageResponse> {
  return read("/queue", isQueuePageResponse, signal);
}

export async function readOverview(
  reviewId: string,
  signal: AbortSignal,
): Promise<ReviewOverviewResponse> {
  const result = await read(
    `/reviews/${encodeURIComponent(reviewId)}`,
    isReviewOverviewResponse,
    signal,
  );
  if (result.data.review.reviewId !== reviewId) throw new Error("The selected review changed.");
  return result;
}

export async function resolveReview(
  reviewId: string,
  runId: string | undefined,
  signal: AbortSignal,
): Promise<ReviewSelectionResponse> {
  const query = new URLSearchParams({ reviewId });
  if (runId !== undefined) query.set("runId", runId);
  const result = await read(`/reviews/resolve?${query}`, isReviewSelectionResponse, signal);
  if (result.data.reviewId !== reviewId || (runId !== undefined && result.data.runId !== runId)) {
    throw new Error("The selected review or run is unavailable.");
  }
  return result;
}

export async function readRun(
  reviewId: string,
  runId: string,
  signal: AbortSignal,
): Promise<RunDetailResponse> {
  const result = await read(`/runs/${encodeURIComponent(runId)}`, isRunDetailResponse, signal);
  if (result.data.reviewId !== reviewId || result.data.runId !== runId) {
    throw new Error("The selected run is unavailable.");
  }
  return result;
}

export async function refreshQueue(): Promise<void> {
  const csrf = await requireSession();
  const response = await fetch("/api/v1/queue/refresh", {
    method: "POST",
    headers: { "X-ReviewQ-CSRF": csrf },
  });
  if (!response.ok) throw await apiError(response);
}

async function read<T>(
  path: string,
  accepts: (data: unknown) => data is T,
  signal: AbortSignal,
): Promise<T> {
  await requireSession();
  const response = await fetch(`/api/v1${path}`, { signal });
  if (!response.ok) throw await apiError(response);
  const payload: unknown = await response.json();
  if (!accepts(payload)) throw new Error("ReviewQ returned an invalid response.");
  return payload;
}

function requireSession(): Promise<string> {
  session ??= bootstrapSession().catch((error: unknown) => {
    session = undefined;
    throw error;
  });
  return session;
}

async function bootstrapSession(): Promise<string> {
  const url = new URL(window.location.href);
  const ticket = new URLSearchParams(url.hash.slice(1)).get("ticket");
  if (ticket !== null) {
    url.hash = "";
    window.history.replaceState(window.history.state, "", url);
  }
  const response = await fetch(
    "/api/v1/session",
    ticket === null
      ? undefined
      : {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ mode: "redeem", ticket }),
        },
  );
  if (response.status === 401)
    throw new Error("Open ReviewQ with the launcher to start a session.");
  if (!response.ok) throw await apiError(response);
  const payload: unknown = await response.json();
  if (
    typeof payload === "object" &&
    payload !== null &&
    "data" in payload &&
    typeof payload.data === "object" &&
    payload.data !== null &&
    "csrfToken" in payload.data &&
    typeof payload.data.csrfToken === "string"
  ) {
    return payload.data.csrfToken;
  }
  throw new Error("ReviewQ returned an invalid session.");
}

async function apiError(response: Response): Promise<Error> {
  const payload: unknown = await response.json().catch(() => null);
  return new Error(
    isFailure(payload) ? payload.error.message : "ReviewQ could not complete the request.",
  );
}
