import { ApiResponseError } from "../../api/client";
import type { Receipt } from "../../api/generated/contracts/launch";

export type PendingLaunch = {
  readonly reviewId: string;
  readonly submissionId: string;
  readonly title: string;
};

const pending = new Map<string, PendingLaunch>();
const listeners = new Set<() => void>();
let snapshot: readonly PendingLaunch[] = [];

export function pendingLaunches(): readonly PendingLaunch[] {
  return snapshot;
}

export function watchPendingLaunches(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function rememberPendingLaunch(value: PendingLaunch): boolean {
  if (pending.has(value.reviewId)) return false;
  pending.set(value.reviewId, value);
  publish();
  return true;
}

export function settleLaunchReceipt(receipt: Receipt) {
  if (receipt.state === "rejected" || (receipt.state === "confirmed" && receipt.runId)) {
    forget(receipt.reviewId, receipt.submissionId);
  }
}

export function settleLaunchRefusal(attempt: PendingLaunch, error: unknown) {
  if (
    error instanceof ApiResponseError &&
    ["invalid_request", "stale_revision", "unauthorized", "forbidden"].includes(error.code)
  ) {
    forget(attempt.reviewId, attempt.submissionId);
    return true;
  }
  return false;
}

function forget(reviewId: string, submissionId: string) {
  if (pending.get(reviewId)?.submissionId !== submissionId) return;
  pending.delete(reviewId);
  publish();
}

function publish() {
  snapshot = [...pending.values()];
  for (const listener of listeners) listener();
}
