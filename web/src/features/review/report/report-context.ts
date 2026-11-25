import type { Descriptor, ReportSummary } from "../../../api/generated/contracts/reports";
import { reportContext } from "../../../api/reports";

// Reads a report's outcomes to completion. The known count must match.
export async function contextItems<T extends Descriptor & { readonly findingId: string }>(
  runId: string,
  report: ReportSummary,
  accepts: (item: Descriptor) => item is T,
  expected: number,
  signal: AbortSignal,
) {
  const items = new Map<string, T>();
  let next: string | undefined;
  do {
    // The server halves a page until it fits the 64 KiB read; this only sets the usual size.
    const result = await reportContext(runId, report.receiptId, "changes", next, signal, 20);
    if (
      result.data.baseReport?.receiptId !== report.receiptId ||
      result.data.baseReport.sha256 !== report.sha256
    ) {
      throw new Error("The displayed report changes changed.");
    }
    for (const item of result.data.items) {
      if (!accepts(item) || items.has(item.findingId))
        throw new Error("The accepted report changes are inconsistent.");
      items.set(item.findingId, item);
    }
    const continuation = result.data.nextCursor;
    if (
      continuation &&
      (!result.data.items.length || continuation === next || items.size >= expected)
    )
      throw new Error("The accepted report changes count changed.");
    next = continuation ?? undefined;
  } while (next);
  if (items.size !== expected) throw new Error("The accepted report changes are incomplete.");
  return items;
}
