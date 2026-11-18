import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { PatchDiscussion } from "../../../api/generated/contracts";
import { mountPatchDocument } from "./patch-document";
import type { PatchDocument } from "./patch-document";

type Props = {
  readonly text: string;
  readonly discussions: readonly PatchDiscussion[];
  readonly restoreScroll: () => number;
  readonly rememberScroll: (scroll: number) => void;
};

export function PatchDocumentView({ text, discussions, restoreScroll, rememberScroll }: Props) {
  const [document, setDocument] = useState<PatchDocument>();
  const mount = useCallback(
    (host: HTMLPreElement | null) => {
      if (!host) return;
      const rendered = mountPatchDocument(host, text);
      host.scrollTop = restoreScroll();
      setDocument(rendered);
      return () => {
        rememberScroll(host.scrollTop);
        host.replaceChildren();
      };
    },
    [text, restoreScroll, rememberScroll],
  );

  return (
    <>
      <pre
        ref={mount}
        className="patch-source patch-rich"
        tabIndex={0}
        aria-label="Selectable unified patch"
      />
      {document &&
        discussions.map((thread) => (
          <ThreadPortal key={thread.discussionId} document={document} thread={thread} />
        ))}
    </>
  );
}

function ThreadPortal({
  document,
  thread,
}: {
  readonly document: PatchDocument;
  readonly thread: PatchDiscussion;
}) {
  const [slot, setSlot] = useState<HTMLElement>();
  const anchor = thread.anchor;
  const line =
    anchor?.newLine !== null && anchor?.newLine !== undefined
      ? `new:${anchor.newLine}`
      : anchor?.oldLine !== null && anchor?.oldLine !== undefined
        ? `old:${anchor.oldLine}`
        : undefined;
  useEffect(() => {
    let current = true;
    const target = line === undefined ? undefined : document.anchor(line);
    queueMicrotask(() => {
      if (current) setSlot(target);
    });
    return () => {
      current = false;
    };
  }, [document, line]);
  return slot ? createPortal(<DiscussionCard thread={thread} />, slot) : null;
}

export function DiscussionCard({ thread }: { readonly thread: PatchDiscussion }) {
  const first = thread.notes[0];
  if (!first) return null;
  return (
    <details className="patch-discussion">
      <summary>
        {first.author} ·{" "}
        {thread.outdated ? "Earlier revision" : first.resolved ? "Resolved" : "Discussion"}
        {thread.notes.length > 1 && ` · ${thread.notes.length} notes`}
      </summary>
      {thread.notes.map((note) => (
        <div key={note.noteId} className="discussion-note">
          <strong>{note.author}</strong>
          <p>{note.body}</p>
        </div>
      ))}
    </details>
  );
}
