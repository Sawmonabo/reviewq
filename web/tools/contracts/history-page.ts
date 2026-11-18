import type { HistoryPageResponse } from "../../src/api/generated/contracts";
import { isHistoryPageResponse } from "../../src/api/generated/validators.mjs";

type HistoryAddress = Pick<HistoryPageResponse["data"], "runId" | "sessionKey">;

const MAX_HISTORY_PAGE_BYTES = 256 * 1024;
const UTF8 = new TextDecoder("utf-8", { fatal: true });

export async function readHistoryPage(
  response: Response,
  address: HistoryAddress,
): Promise<HistoryPageResponse | undefined> {
  if (!response.ok || response.body === null || response.body.locked || response.bodyUsed) {
    return undefined;
  }

  const reader = response.body.getReader();
  let bytes = new Uint8Array();
  let length = 0;
  try {
    for (;;) {
      const chunk = await reader.read();
      if (chunk.done) break;
      if (chunk.value.byteLength > MAX_HISTORY_PAGE_BYTES - length) {
        await reader.cancel();
        return undefined;
      }
      const end = length + chunk.value.byteLength;
      if (end > bytes.byteLength) {
        const capacity = Math.min(MAX_HISTORY_PAGE_BYTES, Math.max(end, bytes.byteLength * 2));
        const grown = new Uint8Array(capacity);
        grown.set(bytes);
        bytes = grown;
      }
      bytes.set(chunk.value, length);
      length = end;
    }
  } catch {
    return undefined;
  } finally {
    reader.releaseLock();
  }

  return decodeHistoryPage(bytes.subarray(0, length), address);
}

export function decodeHistoryPage(
  bytes: Uint8Array,
  address: HistoryAddress,
): HistoryPageResponse | undefined {
  if (bytes.byteLength > MAX_HISTORY_PAGE_BYTES) return undefined;

  try {
    const page: unknown = JSON.parse(UTF8.decode(bytes));
    if (
      isHistoryPageResponse(page) &&
      page.data.runId === address.runId &&
      page.data.sessionKey === address.sessionKey
    ) {
      return page;
    }
  } catch {
    return undefined;
  }
  return undefined;
}
