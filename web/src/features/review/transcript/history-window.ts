import type { HistoryEntry, HistoryPage } from "../../../api/generated/contracts";
import { savedValue, saveValue } from "../../../lib/view-preferences";

export type TimelineBookmark = {
  readonly entryId: string;
  readonly offset: number;
  readonly following: boolean;
  readonly generation: string;
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
    };
  }
}

export function rememberBookmark(key: string, value: TimelineBookmark) {
  saveValue(key, value);
}
