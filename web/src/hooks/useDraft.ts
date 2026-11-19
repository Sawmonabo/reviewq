import { useSyncExternalStore } from "react";
import type { DraftStore } from "../lib/drafts";

export function useDraft(store: DraftStore, key: string) {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  return snapshot.entries.find((entry) => entry.key === key)?.text ?? "";
}
