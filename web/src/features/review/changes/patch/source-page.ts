import {
  isPatchScopeChanged,
  PatchRangeUnavailable,
  readPatch,
  readPatchSource,
} from "../../../../api/changes";
import type { PatchRange, PatchSourceTarget, PatchWindow } from "../../../../api/changes";
import { ApiResponseError } from "../../../../api/client";
import type { PatchFile } from "../../../../api/generated/contracts";
import type { PatchLine } from "../patch-position";
import { patchSourceRows } from "../patch-rows";
import type { PatchRow } from "../patch-rows";

export type SourcePage = PatchWindow & {
  readonly rows?: readonly PatchRow[];
  readonly nextRow?: number;
  readonly continuesLine?: boolean;
};

export type SourceTarget = PatchRange | { readonly row: number; readonly etag: string };

export async function readSourcePage(
  file: PatchFile,
  signal: AbortSignal,
  range: SourceTarget,
  line?: PatchLine,
  recoverEnd = false,
): Promise<SourcePage> {
  if (!file.patchETag) {
    if ("row" in range) throw new Error("The selected patch has no source validator.");
    try {
      return await readPatch(file, signal, range);
    } catch (error) {
      if (!recoverEnd || !(error instanceof PatchRangeUnavailable)) throw error;
      const length = error.reportedLength;
      if (length === undefined || (length > 0 && "last" in range)) throw error;
      signal.throwIfAborted();
      // The failure advertises a length. Only the exact patch response proves its identity.
      const resized = { ...file, byteLength: length, patchETag: range.etag ?? null };
      return readPatch(resized, signal, length === 0 ? undefined : { last: true }).catch(
        (failure: unknown) => {
          signal.throwIfAborted();
          if (isPatchScopeChanged(failure)) throw failure;
          throw failure instanceof ApiResponseError
            ? new PatchRangeUnavailable(undefined, failure)
            : error;
        },
      );
    }
  }
  if (range.etag && range.etag !== file.patchETag)
    throw new Error("The selected patch validator changed.");
  const page = await readPatchSource(file, signal, target(range, line));
  const first = page.rows[0];
  const last = page.rows.at(-1);
  return {
    text: page.rows.map((row) => row.text).join(""),
    first: first?.sourceStart ?? page.totalBytes,
    next: last?.sourceEnd ?? page.totalBytes,
    total: page.totalBytes,
    etag: page.patchETag,
    rows: patchSourceRows(page.rows),
    ...(page.next && {
      nextRow: page.next.row,
      continuesLine: page.next.row === last?.row,
    }),
  };
}

function target(range: SourceTarget, line?: PatchLine): PatchSourceTarget {
  if (line) return { side: line.side, line: line.number };
  if ("row" in range) return { row: range.row };
  if ("offset" in range) return { byte: range.offset };
  if ("before" in range) return { before: range.before };
  return { last: true };
}
