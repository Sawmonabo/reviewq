import { decodeUtf8, readBoundedBytes } from "./bounded-bytes";
import { queryString, read, request, write } from "./client";
import { drawingFontHeaders } from "./drawing-font";
import { validateTablePage } from "./table-page";
import type {
  HistoryContent,
  HistoryNavigation,
  HistoryPage,
  QuestionAnswerRequest,
  SendMessageRequest,
} from "./generated/contracts";
import type { DeliveryReceipt } from "./generated/contracts/delivery";
import type { Form } from "./generated/contracts/questions";
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
export type QuestionAddress = Pick<Form, "requestId" | "turnId" | "itemId">;
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
    (data.tableStart != null && data.tableStart >= byteLength) ||
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
  } else if (data.result.state === "table") {
    validateTablePage(data.result.page, byteEnd - byteStart);
  } else if (length.encode(data.result.markdown).length !== byteEnd - byteStart) {
    throw new Error("The native source range is incomplete.");
  }
  return result;
}

export async function canonicalHistoryMarkdown(
  target: SessionAddress,
  ref: string,
  expected: { readonly sha256: string; readonly byteLength: number },
  signal: AbortSignal,
) {
  if (expected.byteLength < 1 || expected.byteLength > 1024 * 1024)
    throw new Error("The native output exceeds the formatting limit.");
  const response = await request(
    `${path(target)}/history/content/markdown${queryString({ ref, sha256: expected.sha256 })}`,
    { signal },
  );
  if (
    response.headers.get("etag") !== `"${expected.sha256}"` ||
    response.headers.get("content-type")?.split(";", 1)[0] !== "text/markdown"
  )
    throw new Error("The native output source changed. Reopen the entry.");
  const bytes = await readBoundedBytes(response, expected.byteLength);
  if (bytes.length !== expected.byteLength)
    throw new Error("The requested native output was incomplete.");
  return decodeUtf8(bytes);
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
  const result = await read(
    `${path(target)}/messages/${encodeURIComponent(submissionId)}`,
    isDeliveryReceiptResponse,
    signal,
  );
  return { ...result, data: verifyReceipt(target, submissionId, result.data) };
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

export async function readQuestions(
  target: SessionAddress,
  signal: AbortSignal,
  requestId?: string,
) {
  const result = await read(
    `${path(target)}/questions${queryString({ requestId })}`,
    isQuestionFormsResponse,
    signal,
  );
  verify(target, result.data);
  for (const form of result.data.items) verify(target, form);
  return result;
}

export async function readQuestion(
  target: SessionAddress,
  address: QuestionAddress,
  signal: AbortSignal,
) {
  const result = await readQuestions(target, signal, address.requestId);
  const form = result.data.items[0];
  if (result.data.items.length > 1 || (form && !sameQuestion(form, address)))
    throw new Error("The native question changed.");
  return { data: form ?? null, cursor: result.cursor };
}

export async function answerQuestion(
  target: SessionAddress,
  address: QuestionAddress,
  body: Pick<QuestionAnswerRequest, "submissionId" | "answers">,
) {
  const { requestId, turnId, itemId } = address;
  const { data } = await write(
    `${path(target)}/questions/${encodeURIComponent(requestId)}/answer`,
    "POST",
    { ...body, turnId, itemId },
    isQuestionAnswerResponse,
    AbortSignal.timeout(15_000),
  );
  verify(target, data);
  if (!sameQuestion(data, address)) throw new Error("The native question changed.");
  return data;
}

function sameQuestion(left: QuestionAddress, right: QuestionAddress) {
  return (
    left.requestId === right.requestId &&
    left.turnId === right.turnId &&
    left.itemId === right.itemId
  );
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
