import { queryString, read, write } from "./client";
import type { CancelRecoveryRequest, FollowupConfigureRequest, FollowupMonitorRequest, InterruptRequest, ReviewCheckpointRequest, ReviewDecisionRequest } from "./generated/contracts";
import {
  isCancelRecoveryResponse, isFollowupEnrollmentResponse, isFollowupMonitorResponse, isHistoryPlanResponse, isInterruptResponse,
  isRecoveryStateResponse, isReviewDecisionResponse, isTerminalResponse,
} from "./generated/validators.mjs";

export function sessionPath(runId: string, sessionKey: string) {
  return `/runs/${encodeURIComponent(runId)}/sessions/${encodeURIComponent(sessionKey)}`;
}

export async function terminal(runId: string, sessionKey: string, open: boolean) {
  const path = `${sessionPath(runId, sessionKey)}/terminal`;
  const result = open
    ? await write(`${path}/open`, "POST", undefined, isTerminalResponse)
    : await read(path, isTerminalResponse, AbortSignal.timeout(10_000));
  if (result.data.runId !== runId || result.data.sessionKey !== sessionKey) throw new Error("The requested terminal session changed.");
  return result.data;
}

export async function saveDecision(reviewId: string, body: ReviewDecisionRequest | ReviewCheckpointRequest) {
  const result = await write(`/reviews/${encodeURIComponent(reviewId)}/${"disposition" in body ? "decisions" : "checkpoint"}`,
    "POST", body, isReviewDecisionResponse);
  if (result.data.reviewId !== reviewId) throw new Error("The decision review identity changed.");
  return result.data;
}

export function followupEnrollment(reviewId: string, signal: AbortSignal) {
  return read(`/reviews/${encodeURIComponent(reviewId)}/followups`, isFollowupEnrollmentResponse, signal);
}

export async function changeFollowup(reviewId: string, action: "configure" | "pause" | "resume" | "review-now", body: FollowupConfigureRequest | FollowupMonitorRequest) {
  const result = await write(`/reviews/${encodeURIComponent(reviewId)}/followups/${action}`, "POST", body, isFollowupMonitorResponse);
  if (result.data.reviewId !== reviewId || result.data.submissionId !== body.submissionId) throw new Error("The monitor acknowledgement changed.");
  return result.data;
}

export function readRecovery(runId: string, sessionKey: string, signal: AbortSignal) {
  return read(`${sessionPath(runId, sessionKey)}/recovery`, isRecoveryStateResponse, signal);
}

export async function cancelRecovery(runId: string, sessionKey: string, body: CancelRecoveryRequest) {
  const result = await write(`${sessionPath(runId, sessionKey)}/recovery/cancel`, "POST", body, isCancelRecoveryResponse);
  if (result.data.episodeId !== body.episodeId) throw new Error("The recovery episode changed.");
  return result.data;
}

export async function interrupt(runId: string, sessionKey: string, body: InterruptRequest) {
  const result = await write(`${sessionPath(runId, sessionKey)}/interrupt`, "POST", body, isInterruptResponse);
  if (result.data.turnId !== body.expectedTurnId) throw new Error("The active turn changed.");
  return result.data;
}

export async function readPlan(runId: string, sessionKey: string, signal: AbortSignal, cursor?: string) {
  const result = await read(`${sessionPath(runId, sessionKey)}/history/plan${queryString({ cursor })}`, isHistoryPlanResponse, signal);
  if (result.data.runId !== runId || result.data.sessionKey !== sessionKey) throw new Error("The plan belongs to a different conversation.");
  return result;
}
