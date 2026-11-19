import { queryString, read, write } from "./client";
import type {
  ReportDispositionRequest,
  ReportFeedbackRequest,
  ReportViewedRequest,
} from "./generated/contracts";
import {
  isDeliveryReceiptResponse,
  isReportContextResponse,
  isReportDispositionResponse,
  isReportHistoryResponse,
  isReportSectionResponse,
  isReportViewResponse,
  isReportViewedResponse,
} from "./generated/validators.mjs";
import { verifyReceipt } from "./history";
import type { SessionAddress } from "./history";

export async function reportView(
  runId: string,
  signal: AbortSignal,
  receipt?: string,
  view: "summary" | "markdown" = "summary",
) {
  const result = await read(
    `${versionPath(runId, receipt)}${queryString({ view })}`,
    isReportViewResponse,
    signal,
    view === "markdown" ? 7 * 1024 * 1024 : 64 * 1024,
  );
  if (receipt && result.data.report?.receiptId !== receipt)
    throw new Error("The displayed report version changed.");
  return result;
}

export function reportHistory(runId: string, cursor: string | undefined, signal: AbortSignal) {
  return read(
    `${path(runId)}/history${queryString({ cursor })}`,
    isReportHistoryResponse,
    signal,
    256 * 1024,
  );
}

export function reportContext(
  runId: string,
  receiptId: string,
  collection: "findings" | "changes" | "sections" | "dispositions",
  cursor: string | undefined,
  signal: AbortSignal,
) {
  return read(
    `${path(runId)}/context${queryString({ receiptId, collection, cursor })}`,
    isReportContextResponse,
    signal,
    64 * 1024,
  );
}

export async function reportSection(
  runId: string,
  receiptId: string,
  hash: string,
  offset: number,
  signal: AbortSignal,
  characters = 12_000,
) {
  const result = await read(
    `${path(runId)}/section${queryString({ receiptId, offset, characters })}`,
    isReportSectionResponse,
    signal,
    256 * 1024,
  );
  if (
    result.data.receiptId !== receiptId ||
    result.data.sha256 !== hash ||
    result.data.offset !== offset
  )
    throw new Error("The displayed report source changed.");
  return result;
}

export function reportDownload(runId: string, receiptId: string) {
  return `/api/v1${versionPath(runId, receiptId)}/source?download=1`;
}

export async function saveDisposition(runId: string, body: ReportDispositionRequest) {
  const { data } = await write(
    `${path(runId)}/dispositions`,
    "PUT",
    body,
    isReportDispositionResponse,
  );
  if (
    data.receiptId !== body.receiptId ||
    data.finding.kind !== "disposition" ||
    data.finding.findingId !== body.findingId
  ) {
    throw new Error("The finding disposition could not be verified.");
  }
  return data;
}

export async function markReportViewed(runId: string, body: ReportViewedRequest) {
  const { data } = await write(`${path(runId)}/viewed`, "POST", body, isReportViewedResponse);
  if (data.receiptId !== body.receiptId || data.renderedBatchId !== (body.renderedBatchId ?? null))
    throw new Error("The report viewed receipt changed.");
  return data;
}

export async function reportFeedback(target: SessionAddress, body: ReportFeedbackRequest) {
  const { data } = await write(
    `${path(target.runId)}/feedback`,
    "POST",
    body,
    isDeliveryReceiptResponse,
  );
  if (
    data.reportRef?.receiptId !== body.receiptId ||
    data.reportRef?.reportHash !== body.reportHash
  )
    throw new Error("The feedback report reference changed.");
  return verifyReceipt(target, body.submissionId, data);
}

function path(runId: string) {
  return `/runs/${encodeURIComponent(runId)}/report`;
}
function versionPath(runId: string, receipt?: string) {
  return receipt ? `${path(runId)}/versions/${encodeURIComponent(receipt)}` : path(runId);
}
