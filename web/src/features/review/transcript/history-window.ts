import type { HistoryEntry, HistoryPage } from "../../../api/generated/contracts";
import { savedValue, saveValue } from "../../../lib/view-preferences";

export type ReadingAnchor = {
  readonly id: string;
  readonly offset: number;
  readonly toolEntry?: boolean;
  readonly callId?: string;
};

export type TimelineBookmark = {
  readonly entryId: string;
  readonly offset: number;
  readonly following: boolean;
  readonly generation: string;
  readonly toolEntry?: boolean;
  readonly callId?: string;
};

export const PAGE_LIMIT = 6;

export function entriesIn(pages: readonly HistoryPage[]) {
  const seen = new Set<string>();
  const entries: HistoryEntry[] = [];
  for (const page of pages) {
    for (const entry of page.items) {
      if (seen.has(entry.entryId)) continue;
      seen.add(entry.entryId);
      entries.push(entry);
    }
  }
  return entries;
}

export function readBookmark(key: string): TimelineBookmark | undefined {
  const value = savedValue(key);
  if (
    value &&
    typeof value === "object" &&
    "entryId" in value &&
    "offset" in value &&
    "following" in value &&
    "generation" in value &&
    typeof value.entryId === "string" &&
    typeof value.offset === "number" &&
    Number.isFinite(value.offset) &&
    typeof value.following === "boolean" &&
    typeof value.generation === "string"
  ) {
    return {
      entryId: value.entryId,
      offset: value.offset,
      following: value.following,
      generation: value.generation,
      ...("toolEntry" in value && typeof value.toolEntry === "boolean"
        ? { toolEntry: value.toolEntry }
        : {}),
      ...("callId" in value && typeof value.callId === "string" && value.callId.length <= 256
        ? { callId: value.callId }
        : {}),
    };
  }
}

export function rememberBookmark(key: string, value: TimelineBookmark) {
  saveValue(key, value);
}

export function historyBookmark(
  host: HTMLDivElement | null,
  generation: string | undefined,
  following: boolean,
): TimelineBookmark | undefined {
  const current = visibleAnchor(host);
  if (!current || !generation) return;
  return {
    entryId: current.id,
    offset: current.offset,
    following,
    generation,
    toolEntry: current.toolEntry ?? false,
    ...(current.callId ? { callId: current.callId } : {}),
  };
}

export function sameBookmark(left: TimelineBookmark, right: TimelineBookmark | undefined) {
  return (
    right &&
    left.entryId === right.entryId &&
    left.offset === right.offset &&
    left.following === right.following &&
    left.generation === right.generation &&
    left.toolEntry === right.toolEntry &&
    left.callId === right.callId
  );
}

export function visibleAnchor(host: HTMLDivElement | null): ReadingAnchor | undefined {
  if (!host) return;
  const frame = host.getBoundingClientRect();
  const active = document.activeElement?.closest<HTMLElement>("[data-tool-call]");
  const focused = active?.closest<HTMLElement>("[data-native-entry]");
  if (focused && host.contains(focused) && focused.dataset.nativeEntry) {
    const bounds = focused.getBoundingClientRect();
    if (bounds.bottom > frame.top && bounds.top < frame.bottom)
      return {
        id: focused.dataset.nativeEntry,
        offset: bounds.top - frame.top,
        toolEntry: true,
        ...(active?.dataset.toolCall ? { callId: active.dataset.toolCall } : {}),
      };
  }
  for (const node of host.querySelectorAll<HTMLElement>("[data-native-entry]")) {
    const item = node.getBoundingClientRect();
    if (item.bottom > frame.top && item.top < frame.bottom && node.dataset.nativeEntry) {
      const tool = node.querySelector<HTMLElement>("details[open] > [data-tool-call]");
      return {
        id: node.dataset.nativeEntry,
        offset: item.top - frame.top,
        toolEntry:
          node.classList.contains("tool-entry") && node.dataset.entry === node.dataset.nativeEntry,
        ...(tool?.dataset.toolCall ? { callId: tool.dataset.toolCall } : {}),
      };
    }
  }
}
