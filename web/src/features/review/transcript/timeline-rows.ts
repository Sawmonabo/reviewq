import { defaultRangeExtractor } from "@tanstack/react-virtual";
import type { HistoryEntry } from "../../../api/generated/contracts";
import type { HostProviderDto } from "../../../api/generated/contracts/common";
import type { MessageHistoryItem } from "../../../api/generated/contracts/delivery";
import { deliveryRowKey, type ReadingAnchor } from "./history-window";
import { activityRows } from "./tool-activity";
import type { ReadPack, ToolCatalog, ToolRun } from "./tool-activity";

export type ProvisionalReply = { readonly turnId: string; readonly text: string };

type MessageRow = {
  readonly kind: "native" | "live" | "delivery";
  readonly id: string;
  readonly deliveryKey?: string;
  readonly entry?: HistoryEntry;
  readonly delivery?: MessageHistoryItem;
  readonly provisional?: ProvisionalReply;
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
  provider: HostProviderDto,
  older: boolean,
  newer: boolean,
  streaming: boolean,
  groupKeys: ReadonlySet<string>,
): VisibleRow[] {
  const byEntry = new Map(
    messages.flatMap((item) => (item.nativeEntryId ? [[item.nativeEntryId, item] as const] : [])),
  );
  const byMessage = new Map(messages.map((item) => [item.messageId, item] as const));
  const linked = new Set<string>();
  const deliveryKeys = new Set<string>();
  const links = new Map<HistoryEntry, Pick<MessageRow, "delivery" | "deliveryKey">>();
  for (const entry of entries) {
    const confirmed = byEntry.get(entry.entryId);
    const correlated = entry.messageId ? byMessage.get(entry.messageId) : undefined;
    if (!confirmed && !correlated && !entry.messageId) continue;
    if (!entry.parts.some((part) => part.kind === "user_prompt")) continue;
    const exact = confirmed && (!entry.turnId || confirmed.nativeTurnId === entry.turnId);
    const candidate = exact ? confirmed : correlated?.nativeEntryId ? undefined : correlated;
    const delivery = candidate && !linked.has(candidate.messageId) ? candidate : undefined;
    if (delivery) linked.add(delivery.messageId);
    const key = entry.messageId && deliveryRowKey(entry.messageId);
    const deliveryKey =
      key && !deliveryKeys.has(key) && (!correlated?.nativeEntryId || correlated === delivery)
        ? key
        : undefined;
    if (deliveryKey) deliveryKeys.add(deliveryKey);
    links.set(entry, {
      ...(delivery ? { delivery } : {}),
      ...(deliveryKey ? { deliveryKey } : {}),
    });
  }
  const first = entries.find((entry) => entry.timestamp)?.timestamp;
  let last: string | null | undefined;
  for (let i = entries.length - 1; i >= 0 && !last; i--) last = entries[i]?.timestamp;
  const pending = messages
    .filter(
      (item) =>
        !linked.has(item.messageId) &&
        (provider !== "claude" || item.state === "uncertain" || item.state === "rejected") &&
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
      groupKeys,
    )) {
      const link = row.kind === "native" ? links.get(row.entry) : undefined;
      rows.push(link ? { ...row, ...link } : row);
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
    const key = deliveryRowKey(delivery.messageId);
    rows.push({ kind: "delivery", id: key, deliveryKey: key, delivery });
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
      offsets.set(historyId(row), count);
    }
    return count;
  });
  return { count, offsets, ranks };
}

export function readingIndex(
  entries: readonly VisibleRow[],
  saved: ReadingAnchor & { readonly adjacent?: string },
  exact = saved.toolEntry,
) {
  if (saved.rowId) return entries.findIndex((row) => row.id === saved.rowId);
  const index = entries.findIndex((row) => (exact ? row.id : historyId(row)) === saved.id);
  if (index >= 0 || (exact && !saved.adjacent)) return index;
  // A retained native entry can become part of a differently keyed collapsed group.
  return entries.findIndex((row) => row.kind === "tool-run" && row.members.includes(saved.id));
}

export function historyId(row: VisibleRow) {
  return "anchorId" in row ? row.anchorId : (row.entry?.entryId ?? row.id);
}

export function timelineRowKey(row: VisibleRow) {
  return "deliveryKey" in row ? (row.deliveryKey ?? row.id) : row.id;
}

export function isPinnedRow(row: VisibleRow, pinned: ReadonlySet<string>) {
  return pinned.has(row.id) || pinned.has(historyId(row)) || pinned.has(timelineRowKey(row));
}

export function toggleKey(known: ReadonlySet<string>, key: string, open: boolean) {
  if (known.has(key) === open) return known;
  const next = new Set(known);
  if (open) next.add(key);
  else next.delete(key);
  return next.size > 160 ? new Set([...next].slice(-160)) : next;
}

export function openToolRun(
  rows: readonly VisibleRow[],
  known: ReadonlyMap<string, boolean>,
  entryId: string,
) {
  const containing = rows.find((row) => row.kind === "tool-run" && row.members.includes(entryId));
  const next = new Map(known);
  if (containing?.kind === "tool-run") for (const member of containing.members) next.delete(member);
  next.set(entryId, true);
  return new Map([...next].slice(-160));
}

export function transcriptRangeIndexes(
  range: Parameters<typeof defaultRangeExtractor>[0],
  entries: readonly VisibleRow[],
  pinned: ReadonlySet<string>,
  reading: (ReadingAnchor & { readonly adjacent?: string }) | undefined,
  findEntry: string | undefined,
) {
  const indexes = new Set(defaultRangeExtractor(range));
  if (reading?.adjacent) {
    const index = readingIndex(entries, reading);
    if (index >= 0 && !indexes.has(index)) {
      const endIndex = Math.min(entries.length - 1, index + range.endIndex - range.startIndex);
      for (const nearby of defaultRangeExtractor({ ...range, startIndex: index, endIndex }))
        indexes.add(nearby);
    }
  }
  if (pinned.size || findEntry)
    entries.forEach((entry, index) => {
      if (isPinnedRow(entry, pinned) || entry.id === findEntry) indexes.add(index);
    });
  return [...indexes].sort((left, right) => left - right);
}
