import { read, write } from "./client";
import type { RegisterSessionRequest, RunCatalogRequest, RunPrepareRequest, RunStartRequest } from "./generated/contracts";
import { isLaunchReceiptResponse, isRegisteredSessionResponse, isRunCatalogResponse, isRunPreviewResponse } from "./generated/validators.mjs";

export async function runCatalog(body: RunCatalogRequest, signal: AbortSignal) {
  const result = await write("/runs/catalog", "POST", body, isRunCatalogResponse, signal);
  if (result.data.reviewId !== body.reviewId || result.data.provider !== body.provider) throw new Error("The launch catalog changed.");
  return result;
}

export async function prepareRun(body: RunPrepareRequest, signal: AbortSignal) {
  const result = await write("/runs/prepare", "POST", body, isRunPreviewResponse, signal);
  if (result.data.reviewId !== body.reviewId || result.data.provider !== body.provider) throw new Error("The review preview changed.");
  return result.data;
}

export async function startRun(body: RunStartRequest) {
  return verifyLaunch(await write("/runs/start", "POST", body, isLaunchReceiptResponse), body.reviewId, body.submissionId);
}

export async function launchReceipt(reviewId: string, submissionId: string, signal: AbortSignal) {
  const result = await read(`/reviews/${encodeURIComponent(reviewId)}/launches/${encodeURIComponent(submissionId)}`, isLaunchReceiptResponse, signal);
  return verifyLaunch(result, reviewId, submissionId);
}

export async function registerRun(body: RegisterSessionRequest) {
  const { data } = await write("/sessions/register", "POST", body, isRegisteredSessionResponse);
  if (data.provider !== body.provider || data.reviewId !== body.reviewId || data.nativeId !== body.nativeId) throw new Error("The registered session identity changed.");
  return data;
}

function verifyLaunch(result: import("./generated/contracts/launch").LaunchReceiptResponse, reviewId: string, submissionId: string) {
  if (result.data.reviewId !== reviewId || result.data.submissionId !== submissionId) throw new Error("ReviewQ could not confirm this launch receipt.");
  return result.data;
}
