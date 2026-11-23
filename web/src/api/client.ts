import { decodeUtf8, readBoundedBytes } from "./bounded-bytes";
import type {
  ReviewOverviewResponse,
  ReviewSelectionResponse,
  RunDetail,
  RunDetailResponse,
  SessionSummary,
} from "./generated/contracts";
import {
  isFailure,
  isReviewOverviewResponse,
  isReviewSelectionResponse,
  isRunDetailResponse,
} from "./generated/validators.mjs";
import { ApiTransportError } from "./transport-error";

let browserProof: Promise<string> | undefined;

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
  cursor?: string,
): Promise<RunDetailResponse> {
  const result = await read(
    `/runs/${encodeURIComponent(runId)}${queryString({ cursor })}`,
    isRunDetailResponse,
    signal,
  );
  if (result.data.reviewId !== reviewId || result.data.runId !== runId) {
    throw new Error("The selected run is unavailable.");
  }
  return result;
}

export async function findRunSession(
  run: RunDetail,
  sessionKey: string,
  signal: AbortSignal,
): Promise<SessionSummary | null> {
  let page = run.sessions;
  const cursors = new Set<string>();
  for (;;) {
    signal.throwIfAborted();
    const found = page.items.find((session) => session.sessionKey === sessionKey);
    if (found) return found;
    const cursor = page.nextCursor;
    if (!cursor) return null;
    if (cursors.has(cursor)) throw new Error("ReviewQ repeated a session page.");
    cursors.add(cursor);
    const result = await readRun(run.reviewId, run.runId, signal, cursor);
    if (result.data.leadSessionKey !== run.leadSessionKey) {
      throw new Error("The selected lead changed while opening this session.");
    }
    page = result.data.sessions;
  }
}

export async function read<T>(
  path: string,
  accepts: (data: unknown) => data is T,
  signal: AbortSignal,
  limit?: number,
  headers?: HeadersInit,
): Promise<T> {
  return decode(await request(path, { signal, ...(headers ? { headers } : {}) }), accepts, limit);
}

export async function write<T>(
  path: string,
  method: "POST" | "PUT",
  body: unknown,
  accepts: (data: unknown) => data is T,
  signal?: AbortSignal,
): Promise<T> {
  return decode(
    await request(path, {
      method,
      ...(signal ? { signal } : {}),
      ...(body === undefined
        ? {}
        : {
            body: JSON.stringify(body),
            headers: { "Content-Type": "application/json" },
          }),
    }),
    accepts,
  );
}

export async function request(
  path: string,
  options: RequestInit = {},
  allowedStatus?: number,
): Promise<Response> {
  const headers = new Headers(options.headers);
  const method = options.method?.toUpperCase() ?? "GET";
  const reading = method === "GET" || method === "HEAD";
  const url = `/api/v1${path}`;
  let response: Response;
  for (let attempt = 0; ; attempt++) {
    options.signal?.throwIfAborted();
    const active = reading ? undefined : requireBrowserProof();
    if (active) headers.set("X-ReviewQ-CSRF", await active);
    options.signal?.throwIfAborted();
    response = await fetchLocal(url, { ...options, headers });

    // The service marks a proof rejection before running the operation.
    if (
      !active ||
      response.status !== 403 ||
      response.headers.get("X-ReviewQ-CSRF-Rejected") !== "1"
    )
      break;
    if (browserProof === active) browserProof = undefined;
    if (attempt > 0 || (options.body != null && typeof options.body !== "string")) break;
    void response.body?.cancel().catch(() => {});
  }
  if (!response.ok && response.status !== allowedStatus) throw await apiError(response);
  return response;
}

export function queryString(
  values: Readonly<
    Record<string, string | number | boolean | readonly string[] | null | undefined>
  >,
): string {
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

async function decode<T>(
  response: Response,
  accepts: (data: unknown) => data is T,
  limit = 12 * 1024 * 1024,
): Promise<T> {
  const bytes = await readBoundedBytes(response, limit).catch(transportFailure);
  const payload: unknown = JSON.parse(decodeUtf8(bytes));
  if (!accepts(payload)) throw new Error("ReviewQ returned an invalid response.");
  return payload;
}

function requireBrowserProof(): Promise<string> {
  browserProof ??= readBrowserProof().catch((error: unknown) => {
    browserProof = undefined;
    throw error;
  });
  return browserProof;
}

async function readBrowserProof(): Promise<string> {
  const response = await fetchLocal("/api/v1/session", { cache: "no-store" });
  if (!response.ok) throw await apiError(response);
  const payload: unknown = await response.json().catch(transportFailure);
  if (
    typeof payload === "object" &&
    payload !== null &&
    "data" in payload &&
    typeof payload.data === "object" &&
    payload.data !== null &&
    "csrfToken" in payload.data &&
    typeof payload.data.csrfToken === "string" &&
    /^[A-Za-z0-9_-]{43}$/.test(payload.data.csrfToken)
  ) {
    return payload.data.csrfToken;
  }
  throw new Error("ReviewQ returned an invalid browser proof.");
}

function fetchLocal(url: string, options: RequestInit): Promise<Response> {
  return fetch(url, {
    ...options,
    mode: "same-origin",
    credentials: "omit",
    redirect: "error",
  }).catch(transportFailure);
}

function transportFailure(error: unknown): never {
  if (error instanceof TypeError) throw new ApiTransportError("Could not reach ReviewQ.");
  throw error;
}

async function apiError(response: Response): Promise<Error> {
  const payload: unknown = await response.json().catch((error: unknown) => {
    if (error instanceof DOMException && error.name === "AbortError") throw error;
    return null;
  });
  if (isFailure(payload)) {
    return new ApiResponseError(payload.error.code, payload.error.message, payload.error.retryable);
  }
  if (response.status >= 500)
    return new ApiTransportError("ReviewQ is unavailable.", response.status);
  return new Error("ReviewQ could not complete the request.");
}

export class ApiResponseError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly retryable: boolean,
  ) {
    super(message);
  }
}
