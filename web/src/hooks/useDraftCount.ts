import { createContext, useContext, useSyncExternalStore } from "react";
import type { DraftStore } from "../lib/drafts";

// The shell's draft store, for screens that only need to know how many unsent drafts it holds.
export const DraftsContext = createContext<DraftStore | undefined>(undefined);

export function useDraftCount() {
  const store = useContext(DraftsContext);
  return useSyncExternalStore(store?.subscribe ?? subscribeNothing, store?.count ?? noDrafts);
}

function subscribeNothing() {
  return () => {};
}

function noDrafts() {
  return 0;
}
