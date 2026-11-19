import { decodeUtf8, readBoundedBytes } from "./bounded-bytes";
import type {
  ReviewOverviewResponse,
  ReviewSelectionResponse,
  RunDetailResponse,
} from "./generated/contracts";
import {
  isFailure,
  isReviewOverviewResponse,
  isReviewSelectionResponse,
  isRunDetailResponse,
} from "./generated/validators.mjs";

let session: Promise<string> | undefined;

export async function readOverview(
  reviewId: string,
  signal: AbortSignal,
  cursor?: string,
): Promise<ReviewOverviewResponse> {
  const result = await read(
    `/reviews/${encodeURIComponent(reviewId)}${queryString({ cursor })}`,
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

export async function read<T>(
  path: string,
  accepts: (data: unknown) => data is T,
  signal: AbortSignal,
): Promise<T> {
  return decode(await request(path, { signal }), accepts);
}

export async function write<T>(
  path: string,
  method: "POST" | "PUT",
  body: unknown,
  accepts: (data: unknown) => data is T,
  signal?: AbortSignal,
): Promise<T> {
  return decode(await request(path, {
    method,
    ...(signal ? { signal } : {}),
    ...(body === undefined ? {} : {
      body: JSON.stringify(body),
      headers: { "Content-Type": "application/json" },
    }),
  }), accepts);
}

export async function request(path: string, options: RequestInit = {}): Promise<Response> {
  const token = await requireSession();
  const headers = new Headers(options.headers);
  if (options.method && options.method !== "GET") headers.set("X-ReviewQ-CSRF", token);
  const response = await fetch(`/api/v1${path}`, { ...options, headers });
  if (response.status === 401) session = undefined;
  if (!response.ok) throw await apiError(response);
  return response;
}

export function queryString(values: Readonly<Record<string, string | number | boolean | readonly string[] | null | undefined>>): string {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(values)) {
    if (value === null || value === undefined) continue;
    if (typeof value === "object") {
      for (const item of value) params.append(key, item);
    } else {
      params.set(key, String(value));
    }
  }
  return params.size ? `?${params}` : "";
}

async function decode<T>(response: Response, accepts: (data: unknown) => data is T): Promise<T> {
  const payload: unknown = JSON.parse(decodeUtf8(await readBoundedBytes(response, 12 * 1024 * 1024)));
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
  if (isFailure(payload)) {
    return new ApiResponseError(payload.error.code, payload.error.message, payload.error.retryable);
  }
  return new Error("ReviewQ could not complete the request.");
}

export class ApiResponseError extends Error {
  constructor(readonly code: string, message: string, readonly retryable: boolean) {
    super(message);
  }
}
