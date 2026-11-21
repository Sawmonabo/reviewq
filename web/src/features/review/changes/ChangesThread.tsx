import { createContext, useContext, useMemo, useState } from "react";
import type { ReactNode } from "react";
import type {
  DiscussionDestination,
  PatchDiscussion,
  PatchDiscussionNote,
} from "../../../api/generated/contracts";
import { DiscussionThread } from "../DiscussionThread";

type Expansion = {
  readonly state: ReadonlyMap<string, boolean>;
  readonly update: (id: string, open: boolean) => void;
  readonly target: DiscussionDestination | undefined;
};
const threadExpansion = createContext<Expansion | undefined>(undefined);

export function ChangesThreadExpansion({
  children,
  target,
}: {
  readonly children: ReactNode;
  readonly target?: DiscussionDestination | undefined;
}) {
  const [state, setState] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [opening, setOpening] = useState<DiscussionDestination>();
  if (opening !== target) {
    setOpening(target);
    if (target) setState(new Map(state).set(target.thread.discussionId, true));
  }
  const expansion = useMemo(
    () => ({
      state,
      target,
      update(id: string, open: boolean) {
        setState((previous) =>
          previous.get(id) === open ? previous : new Map(previous).set(id, open),
        );
      },
    }),
    [state, target],
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
  const target =
    expansion?.target?.thread.discussionId === thread.discussionId ? expansion.target : undefined;
  const expanded = expansion?.state.get(thread.discussionId) ?? thread.resolution !== "resolved";
  return (
    <DiscussionThread
      thread={thread}
      outside={outside}
      previewNote={previewNote}
      presentation="changes"
      onJump={onJump}
      expanded={expanded}
      selectedNote={target?.noteId}
      draft={target?.kind === "draft"}
      onExpanded={(open) => {
        if (expanded !== open) expansion?.update(thread.discussionId, open);
      }}
    />
  );
}

export function useExpandedChangesThread(id: string) {
  return useContext(threadExpansion)?.state.get(id) === true;
}
