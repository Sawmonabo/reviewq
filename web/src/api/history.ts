import { queryString, read, write } from "./client";
import type { HistoryContent, HistoryNavigation, HistoryPage, QuestionAnswerRequest, SendMessageRequest } from "./generated/contracts";
import type { DeliveryReceipt } from "./generated/contracts/delivery";
import {
  isDeliveryReceiptResponse, isHistoryContentResponse, isHistoryNavigationResponse, isHistoryPageResponse, isHistoryPartPageResponse,
  isMessageHistoryPageResponse, isQuestionAnswerResponse, isQuestionFormsResponse,
} from "./generated/validators.mjs";
import { sessionPath } from "./review";

export type SessionAddress = { readonly runId: string; readonly sessionKey: string };
type ReadAddress = { readonly runId: string; readonly sessionKey: string };

export async function historyPage(target: SessionAddress, params: { cursor?: string; aroundId?: string; locator?: string }, signal: AbortSignal) {
  const result = await read(`${path(target)}/history/page${queryString(params)}`, isHistoryPageResponse, signal, 65_536);
  verify(target, result.data);
  return result;
}

export async function historyContent(target: SessionAddress, entryId: string, generation: string, reference: string, cursor: string | undefined, signal: AbortSignal) {
  const { data } = await read(`${path(target)}/history/content${queryString({ ref: reference, cursor })}`, isHistoryContentResponse, signal, 384 * 1024);
  verify(target, data);
  if (data.entryId !== entryId || data.generation !== generation || data.reset) throw new Error("The native output source changed. Reopen the entry.");
  return data;
}

export async function historyParts(target: SessionAddress, entryId: string, cursor: string, signal: AbortSignal) {
  const { data } = await read(`${path(target)}/history/parts${queryString({ cursor })}`, isHistoryPartPageResponse, signal, 128 * 1024);
  if (data.entryId !== entryId) throw new Error("The native output entry changed.");
  return data;
}

export async function historyPrompts(target: SessionAddress, params: { cursor?: string; overviewCursor?: string; limit?: number; maxGroups?: number }, signal: AbortSignal) {
  const result = await read(`${path(target)}/history/navigation${queryString(params)}`, isHistoryNavigationResponse, signal, 64 * 1024);
  verify(target, result.data);
  return result;
}

export function nativeHistoryUrl(target: SessionAddress) {
  return `/api/v1${path(target)}/history/stream`;
}

export function messageHistory(target: SessionAddress, cursor: string | undefined, signal: AbortSignal) {
  return read(`${path(target)}/messages${queryString({ cursor, limit: 12 })}`, isMessageHistoryPageResponse, signal, 1024 * 1024);
}

export async function sendMessage(target: SessionAddress, body: SendMessageRequest) {
  const { data } = await write(`${path(target)}/messages`, "POST", body, isDeliveryReceiptResponse);
  return verifyReceipt(target, body.submissionId, data);
}

export async function readReceipt(target: SessionAddress, submissionId: string, signal: AbortSignal) {
  const { data } = await read(`${path(target)}/messages/${encodeURIComponent(submissionId)}`, isDeliveryReceiptResponse, signal);
  return verifyReceipt(target, submissionId, data);
}

export function verifyReceipt(target: SessionAddress, submissionId: string, result: DeliveryReceipt) {
  verify(target, result);
  if (result.submissionId !== submissionId) throw new Error("The native delivery receipt changed.");
  return result;
}

export async function readQuestions(target: SessionAddress, signal: AbortSignal) {
  const result = await read(`${path(target)}/questions`, isQuestionFormsResponse, signal);
  verify(target, result.data);
  for (const form of result.data.items) verify(target, form);
  return result;
}

export async function answerQuestion(target: SessionAddress, requestId: string, turnId: string, itemId: string, body: QuestionAnswerRequest) {
  const { data } = await write(`${path(target)}/questions/${encodeURIComponent(requestId)}/answer`, "POST", body, isQuestionAnswerResponse);
  verify(target, data);
  if (data.requestId !== requestId || data.turnId !== turnId || data.itemId !== itemId) throw new Error("The native question changed.");
  return data;
}

function path(target: SessionAddress) {
  return sessionPath(target.runId, target.sessionKey);
}

function verify(target: SessionAddress, value: HistoryPage | HistoryNavigation | HistoryContent | ReadAddress) {
  if (value.runId !== target.runId || value.sessionKey !== target.sessionKey) throw new Error("The selected native conversation changed.");
}
