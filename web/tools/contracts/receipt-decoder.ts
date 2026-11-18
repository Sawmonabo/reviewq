import type { DeliveryReceiptResponse, Failure } from "../../../contracts/generated/contracts";
import { isDeliveryReceiptResponse, isFailure } from "../../../contracts/generated/validators.mjs";

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

function parse(bytes: string): unknown {
  try {
    return JSON.parse(bytes);
  } catch {
    return undefined;
  }
}
