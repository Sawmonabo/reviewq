import { queryString, read, write } from "./client";
import { drawingFontHeaders } from "./drawing-font";
import type {
  HistoryContent,
  HistoryNavigation,
  HistoryPage,
  QuestionAnswerRequest,
  SendMessageRequest,
} from "./generated/contracts";
import type { DeliveryReceipt } from "./generated/contracts/delivery";
import {
  isDeliveryReceiptResponse,
  isHistoryContentResponse,
  isHistoryDocumentResponse,
  isHistoryNavigationResponse,
  isHistoryPageResponse,
  isHistoryPartPageResponse,
  isHistoryResultResponse,
  isHistorySearchDestinationResponse,
  isHistorySearchResponse,
  isMessageHistoryPageResponse,
  isQuestionAnswerResponse,
  isQuestionFormsResponse,
} from "./generated/validators.mjs";
import { sessionPath } from "./review";

export type SessionAddress = { readonly runId: string; readonly sessionKey: string };
type ReadAddress = { readonly runId: string; readonly sessionKey: string };

export async function historyPage(
  target: SessionAddress,
  params: { cursor?: string; aroundId?: string; locator?: string; limit?: number },
  signal: AbortSignal,
) {
  const result = await read(
    `${path(target)}/history/page${queryString(params)}`,
    isHistoryPageResponse,
    signal,
    65_536,
    drawingFontHeaders(),
  );
  verify(target, result.data);
  return result;
}

export async function historyContent(
  target: SessionAddress,
  entryId: string,
  generation: string,
  reference: string,
  cursor: string | undefined,
  signal: AbortSignal,
) {
  const { data } = await read(
    `${path(target)}/history/content${queryString({ ref: reference, cursor })}`,
    isHistoryContentResponse,
    signal,
    384 * 1024,
    drawingFontHeaders(),
  );
  verify(target, data);
  if (data.entryId !== entryId || data.generation !== generation || data.reset)
    throw new Error("The native output source changed. Reopen the entry.");
  return data;
}

const length = new TextEncoder();
const MAX_DOCUMENT_WINDOW_BYTES = 65_536;

export async function historyDocument(
  target: SessionAddress,
  entryId: string,
  generation: string,
  ref: string,
  position: { byteOffset: number; sha256?: string },
  signal: AbortSignal,
) {
  const result = await read(
    `${path(target)}/history/content/window${queryString({ ref, ...position })}`,
    isHistoryDocumentResponse,
    signal,
    640 * 1024,
    drawingFontHeaders(),
  );
  const { data } = result;
  verify(target, data);
  const { byteStart, byteEnd, byteLength, nextByte, previousByte } = data;
  if (
    data.entryId !== entryId ||
    data.generation !== generation ||
    (position.sha256 !== undefined && position.sha256 !== data.sha256)
  )
    throw new Error("The native output source changed. Reopen the entry.");
  if (
    data.indexFormat !== 2 ||
    byteEnd > byteLength ||
    byteEnd < byteStart ||
    byteEnd - byteStart > MAX_DOCUMENT_WINDOW_BYTES ||
    !(byteStart <= position.byteOffset && position.byteOffset <= byteEnd) ||
    (nextByte !== null && (nextByte !== byteEnd || nextByte >= byteLength)) ||
    (previousByte !== null && previousByte >= byteStart) ||
    (previousByte === null && byteStart !== 0) ||
    (nextByte === null && byteEnd !== byteLength)
  )
    throw new Error("The native document range is invalid.");
  if (data.result.state === "ready") {
    let byte = byteStart;
    for (const block of data.result.blocks) {
      if (
        block.byteStart !== byte ||
        block.byteEnd <= byte ||
        length.encode(block.markdown).length !== block.byteEnd - byte
      )
        throw new Error("The native document block is invalid.");
      byte = block.byteEnd;
    }
    if (byte !== byteEnd) throw new Error("The native document range is incomplete.");
  } else if (length.encode(data.result.markdown).length !== byteEnd - byteStart) {
    throw new Error("The native source range is incomplete.");
  }
  return result;
}

export async function historyResult(target: SessionAddress, signal: AbortSignal) {
  const result = await read(
    `${path(target)}/history/result`,
    isHistoryResultResponse,
    signal,
    65_536,
    drawingFontHeaders(),
  );
  verify(target, result.data);
  if (result.data.entry && !result.data.generation)
    throw new Error("The final response source changed.");
  return result;
}

export async function historyParts(
  target: SessionAddress,
  entryId: string,
  cursor: string,
  signal: AbortSignal,
) {
  const { data } = await read(
    `${path(target)}/history/parts${queryString({ cursor })}`,
    isHistoryPartPageResponse,
    signal,
    128 * 1024,
    drawingFontHeaders(),
  );
  if (data.entryId !== entryId) throw new Error("The native output entry changed.");
  return data;
}

export async function historyPrompts(
  target: SessionAddress,
  params: { cursor?: string; overviewCursor?: string; limit?: number; maxGroups?: number },
  signal: AbortSignal,
) {
  const result = await read(
    `${path(target)}/history/navigation${queryString(params)}`,
    isHistoryNavigationResponse,
    signal,
    64 * 1024,
  );
  verify(target, result.data);
  return result;
}

export async function historySearch(
  target: SessionAddress,
  params: { q: string; cursor?: string; reverse?: boolean },
  signal: AbortSignal,
) {
  const { data } = await read(
    `${path(target)}/history/search${queryString(params)}`,
    isHistorySearchResponse,
    signal,
    256 * 1024,
  );
  verify(target, data);
  return data;
}

export async function historySearchDestination(
  target: SessionAddress,
  hit: string,
  signal: AbortSignal,
) {
  const { data } = await read(
    `${path(target)}/history/search/locate${queryString({ hit })}`,
    isHistorySearchDestinationResponse,
    signal,
    16 * 1024,
  );
  verify(target, data);
  return data;
}

export function nativeHistoryUrl(target: SessionAddress) {
  return `/api/v1${path(target)}/history/stream`;
}

export function messageHistory(
  target: SessionAddress,
  cursor: string | undefined,
  signal: AbortSignal,
) {
  return read(
    `${path(target)}/messages${queryString({ cursor, limit: 12 })}`,
    isMessageHistoryPageResponse,
    signal,
    1024 * 1024,
  );
}

export async function sendMessage(target: SessionAddress, body: SendMessageRequest) {
  const { data } = await write(`${path(target)}/messages`, "POST", body, isDeliveryReceiptResponse);
  return verifyReceipt(target, body.submissionId, data);
}

export async function readReceipt(
  target: SessionAddress,
  submissionId: string,
  signal: AbortSignal,
) {
  const { data } = await read(
    `${path(target)}/messages/${encodeURIComponent(submissionId)}`,
    isDeliveryReceiptResponse,
    signal,
  );
  return verifyReceipt(target, submissionId, data);
}

export function verifyReceipt(
  target: SessionAddress,
  submissionId: string,
  result: DeliveryReceipt,
) {
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

export async function answerQuestion(
  target: SessionAddress,
  requestId: string,
  turnId: string,
  itemId: string,
  body: QuestionAnswerRequest,
) {
  const { data } = await write(
    `${path(target)}/questions/${encodeURIComponent(requestId)}/answer`,
    "POST",
    body,
    isQuestionAnswerResponse,
  );
  verify(target, data);
  if (data.requestId !== requestId || data.turnId !== turnId || data.itemId !== itemId)
    throw new Error("The native question changed.");
  return data;
}

function path(target: SessionAddress) {
  return sessionPath(target.runId, target.sessionKey);
}

function verify(
  target: SessionAddress,
  value: HistoryPage | HistoryNavigation | HistoryContent | ReadAddress,
) {
  if (value.runId !== target.runId || value.sessionKey !== target.sessionKey)
    throw new Error("The selected native conversation changed.");
}
