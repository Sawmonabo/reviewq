import { read, request, write } from "./client";
import type { LoginStartRequest, UninstallRequest } from "./generated/contracts";
import { isLoginStartResponse, isSystemFactsResponse } from "./generated/validators.mjs";
import { ApiTransportError } from "./transport-error";

export function readSystem(signal: AbortSignal) {
  return read("/system", isSystemFactsResponse, signal);
}

export function setLoginStart(body: LoginStartRequest) {
  return write("/system/login-start", "POST", body, isLoginStartResponse);
}

export async function quitReviewQ() {
  await request("/system/quit", { method: "POST" });
}

export async function uninstallReviewQ(body: UninstallRequest) {
  await request("/system/uninstall", {
    method: "POST",
    body: JSON.stringify(body),
    headers: { "Content-Type": "application/json" },
  });
}

export function lostReply(error: unknown) {
  return error instanceof ApiTransportError && error.status === undefined;
}

// After a lost reply, one read tells whether ReviewQ is still running. Any HTTP answer counts.
export async function stillAnswers() {
  try {
    await readSystem(AbortSignal.timeout(5000));
    return true;
  } catch (error) {
    return !lostReply(error) && !(error instanceof DOMException);
  }
}
