import { ApiResponseError } from "../../api/client";
import type { RunStartRequest } from "../../api/generated/contracts";
import type { Receipt } from "../../api/generated/contracts/launch";
import { savedValue, saveValue } from "../../lib/view-preferences";

export type PendingLaunch = {
  readonly reviewId: string;
  readonly submissionId: string;
  readonly title: string;
  readonly request?: RunStartRequest;
};

const bookmark = "launch-receipts";
const pending = restoreLocators();
const listeners = new Set<() => void>();
let snapshot: readonly PendingLaunch[] = [...pending.values()];

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
  const existing = pending.get(value.reviewId);
  if (existing && (existing.submissionId !== value.submissionId || existing.request)) return false;
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
  saveValue(
    bookmark,
    snapshot.map(({ reviewId, submissionId }) => ({ reviewId, submissionId })),
  );
  for (const listener of listeners) listener();
}

function restoreLocators() {
  const references = new Map<string, PendingLaunch>();
  const values = savedValue(bookmark);
  if (!Array.isArray(values)) return references;
  for (const item of values as unknown[]) {
    if (typeof item !== "object" || !item || !("reviewId" in item) || !("submissionId" in item))
      continue;
    const { reviewId, submissionId } = item;
    if (
      typeof reviewId !== "string" ||
      !/^[^\p{Cc}]{1,256}$/u.test(reviewId) ||
      typeof submissionId !== "string" ||
      !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(submissionId)
    )
      continue;
    references.set(reviewId, { reviewId, submissionId, title: reviewId });
  }
  return references;
}
