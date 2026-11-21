import { createContext, useContext, useMemo, useState } from "react";
import type { ReactNode } from "react";
import type { PatchDiscussion, PatchDiscussionNote } from "../../../api/generated/contracts";
import { DiscussionThread } from "../DiscussionThread";

type Expansion = {
  readonly state: ReadonlyMap<string, boolean>;
  readonly update: (id: string, open: boolean) => void;
};
const threadExpansion = createContext<Expansion | undefined>(undefined);

export function ChangesThreadExpansion({ children }: { readonly children: ReactNode }) {
  const [state, setState] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const expansion = useMemo(
    () => ({
      state,
      update(id: string, open: boolean) {
        setState((previous) =>
          previous.get(id) === open ? previous : new Map(previous).set(id, open),
        );
      },
    }),
    [state],
  );
  return <threadExpansion.Provider value={expansion}>{children}</threadExpansion.Provider>;
}

export function ChangesThread({
  thread,
  outside = false,
  previewNote,
  onJump,
}: {
  readonly thread: PatchDiscussion;
  readonly outside?: boolean;
  readonly previewNote?: ((note: PatchDiscussionNote) => string | undefined) | undefined;
  readonly onJump?: (() => void) | undefined;
}) {
  const expansion = useContext(threadExpansion);
  const expanded = expansion?.state.get(thread.discussionId) ?? thread.resolution !== "resolved";
  return (
    <DiscussionThread
      thread={thread}
      outside={outside}
      previewNote={previewNote}
      presentation="changes"
      onJump={onJump}
      expanded={expanded}
      onExpanded={(open) => {
        if (expanded !== open) expansion?.update(thread.discussionId, open);
      }}
    />
  );
}

export function useExpandedChangesThread(id: string) {
  return useContext(threadExpansion)?.state.get(id) === true;
}
