import type { Descriptor, ReportSummary } from "../../../api/generated/contracts/reports";
import { reportContext } from "../../../api/reports";

// Reads one report context collection to completion. A known count must match.
// The cursor is the first page's, so an invalidation between pages still refreshes.
export async function contextItems<T extends Descriptor & { readonly findingId: string }>(
  runId: string,
  report: ReportSummary,
  collection: "findings" | "changes",
  accepts: (item: Descriptor) => item is T,
  expected: number | null,
  signal: AbortSignal,
) {
  const items = new Map<string, T>();
  let next: string | undefined;
  let cursor: string | undefined;
  do {
    // The server halves a page until it fits the 64 KiB read; this only sets the usual size.
    const limit = collection === "findings" ? 10 : 20;
    const result = await reportContext(runId, report.receiptId, collection, next, signal, limit);
    cursor ??= result.cursor;
    if (
      result.data.baseReport?.receiptId !== report.receiptId ||
      result.data.baseReport.sha256 !== report.sha256
    ) {
      throw new Error(`The displayed report ${collection} changed.`);
    }
    for (const item of result.data.items) {
      if (!accepts(item) || items.has(item.findingId))
        throw new Error(`The accepted report ${collection} are inconsistent.`);
      items.set(item.findingId, item);
    }
    const continuation = result.data.nextCursor;
    if (
      continuation &&
      (!result.data.items.length ||
        continuation === next ||
        (expected !== null && items.size >= expected))
    )
      throw new Error(`The accepted report ${collection} count changed.`);
    next = continuation ?? undefined;
  } while (next);
  if (expected !== null && items.size !== expected)
    throw new Error(`The accepted report ${collection} are incomplete.`);
  return { items, cursor: cursor ?? "" };
}
