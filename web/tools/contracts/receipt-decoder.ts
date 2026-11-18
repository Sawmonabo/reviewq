import type {
  DeliveryReceiptResponse,
  Failure,
  HistoryPageResponse,
} from "../../../contracts/generated/contracts";
import {
  isDeliveryReceiptResponse,
  isEventReplayCursor,
  isFailure,
  isInvalidateEvent,
  isPatchFileIndexResponse,
  isQueuePageResponse,
  isResetEvent,
  isReviewOverviewResponse,
  isReviewSelectionResponse,
  isRunDetailResponse,
  isSendMessageRequest,
} from "../../../contracts/generated/validators.mjs";
import { decodeHistoryPage, readHistoryPage } from "./history-page";

type Address = Pick<DeliveryReceiptResponse["data"], "submissionId" | "runId" | "sessionKey">;

export function decodeDeliveryReceipt(
  bytes: string,
  address: Address,
): DeliveryReceiptResponse | undefined {
  const response = parse(bytes);
  if (
    isDeliveryReceiptResponse(response) &&
    response.data.submissionId === address.submissionId &&
    response.data.runId === address.runId &&
    response.data.sessionKey === address.sessionKey
  ) {
    return response;
  }
  return undefined;
}

export function decodeFailure(bytes: string): Failure | undefined {
  const response = parse(bytes);
  return isFailure(response) ? response : undefined;
}

export function findObservedInput(
  receipt: DeliveryReceiptResponse["data"],
  history: HistoryPageResponse["data"],
) {
  if (
    receipt.state !== "started" ||
    receipt.runId !== history.runId ||
    receipt.sessionKey !== history.sessionKey
  ) {
    return undefined;
  }
  const observed = receipt.observedInput;
  return history.items.find(
    (item) =>
      item.entryId === observed.entryId &&
      item.turnId === observed.turnId &&
      item.parts.some((part) => part.kind === "user_prompt"),
  );
}

export const firstReviewDecoders = {
  queue: (bytes: string) => decode(bytes, isQueuePageResponse),
  overview: (bytes: string) => decode(bytes, isReviewOverviewResponse),
  selection(bytes: string, address: { readonly reviewId: string; readonly runId?: string }) {
    const response = decode(bytes, isReviewSelectionResponse);
    if (
      response?.data.reviewId === address.reviewId &&
      (address.runId === undefined || response.data.runId === address.runId)
    ) {
      return response;
    }
    return undefined;
  },
  run: (bytes: string) => decode(bytes, isRunDetailResponse),
  patch(bytes: string, address: { readonly reviewId: string; readonly revision: string }) {
    const response = decode(bytes, isPatchFileIndexResponse);
    if (
      response?.data.revision === address.revision &&
      response.data.items.every(
        (file) => file.reviewId === address.reviewId && file.revision === address.revision,
      )
    ) {
      return response;
    }
    return undefined;
  },
  history: decodeHistoryPage,
  readHistory: readHistoryPage,
  sendRequest: (bytes: string) => decode(bytes, isSendMessageRequest),
  eventReplayCursor: (bytes: string) => decode(bytes, isEventReplayCursor),
  invalidate: (bytes: string) => decode(bytes, isInvalidateEvent),
  reset: (bytes: string) => decode(bytes, isResetEvent),
};

function decode<T>(bytes: string, accepts: (value: unknown) => value is T): T | undefined {
  const value = parse(bytes);
  return accepts(value) ? value : undefined;
}

function parse(bytes: string): unknown {
  try {
    return JSON.parse(bytes);
  } catch {
    return undefined;
  }
}
