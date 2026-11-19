import { useSyncExternalStore } from "react";
import type { DraftStore } from "../lib/drafts";

export function useDraft(store: DraftStore, key: string) {
  return useSyncExternalStore(store.subscribe, () => store.getSnapshot().entries.find((entry) => entry.key === key)?.text ?? "");
}
