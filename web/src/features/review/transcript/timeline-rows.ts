import type { HistoryEntry } from "../../../api/generated/contracts";
import type { MessageHistoryItem } from "../../../api/generated/contracts/delivery";
import { activityRows } from "./tool-activity";
import type { ReadPack, ToolCatalog, ToolRun } from "./tool-activity";

type MessageRow = {
  readonly kind: "native" | "live" | "delivery";
  readonly id: string;
  readonly entry?: HistoryEntry;
  readonly delivery?: MessageHistoryItem;
  readonly provisional?: string;
  readonly grouped?: boolean;
};
export type VisibleRow = MessageRow | ToolRun | ReadPack;

export function timelineRows(
  entries: readonly HistoryEntry[],
  tools: ToolCatalog,
  expandedRuns: ReadonlyMap<string, boolean>,
  expandedReads: ReadonlySet<string>,
  expandedCalls: ReadonlySet<string>,
  messages: readonly MessageHistoryItem[],
  older: boolean,
  newer: boolean,
  streaming: boolean,
): VisibleRow[] {
  const confirmed = new Map(
    messages.flatMap((item) => (item.nativeEntryId ? [[item.nativeEntryId, item] as const] : [])),
  );
  const loadedIds = new Set(entries.map((entry) => entry.entryId));
  const first = entries.find((entry) => entry.timestamp)?.timestamp;
  let last: string | null | undefined;
  for (let i = entries.length - 1; i >= 0 && !last; i--) last = entries[i]?.timestamp;
  const pending = messages
    .filter(
      (item) =>
        (!item.nativeEntryId || !loadedIds.has(item.nativeEntryId)) &&
        (!older || (first && item.createdAt >= first)) &&
        (!newer || (last && item.createdAt <= last)),
    )
    .sort((left, right) => left.createdAt.localeCompare(right.createdAt));
  const rows: VisibleRow[] = [];
  let start = 0;
  function addNative(end: number) {
    if (end <= start) return;
    const span = start === 0 && end === entries.length ? entries : entries.slice(start, end);
    for (const row of activityRows(
      span,
      tools,
      expandedRuns,
      expandedReads,
      expandedCalls,
      older && start === 0,
      (newer || streaming) && end === entries.length,
    )) {
      const delivery = row.kind === "native" ? confirmed.get(row.id) : undefined;
      rows.push(row.kind === "native" && delivery ? { ...row, delivery } : row);
    }
    start = end;
  }
  let index = 0;
  for (const delivery of pending) {
    while (index < entries.length) {
      const time = entries[index]?.timestamp;
      if (time && time > delivery.createdAt) break;
      index++;
    }
    addNative(index);
    rows.push({ kind: "delivery", id: `delivery:${delivery.messageId}`, delivery });
  }
  addNative(entries.length);
  return rows;
}

// Prefix counts let the rail follow scrolling without rescanning native messages on each frame.
export function promptRanks(rows: readonly VisibleRow[]) {
  let count = 0;
  const offsets = new Map<string, number>();
  const ranks = rows.map((row) => {
    if (row.kind === "native" && row.entry?.parts.some((part) => part.kind === "user_prompt")) {
      count++;
      offsets.set(row.id, count);
    }
    return count;
  });
  return { count, offsets, ranks };
}
