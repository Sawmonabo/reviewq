import { useCallback, useState } from "react";
import { createPortal } from "react-dom";
import type { PatchDiscussion } from "../../../api/generated/contracts";
import { ChangesThread } from "./ChangesThread";
import { mountPatchDocument } from "./patch-document";
import type { PatchDocument } from "./patch-document";
import { threadLine } from "./patch-position";

type Props = {
  readonly text: string;
  readonly discussions: readonly PatchDiscussion[];
};

export function PatchDocumentView({ text, discussions }: Props) {
  const [patch, setPatch] = useState<PatchDocument>();
  const mount = useCallback(
    (host: HTMLDivElement | null) => {
      if (!host) return;
      const rendered = mountPatchDocument(host, text);
      setPatch(rendered);
      return rendered.release;
    },
    [text],
  );
  return (
    <>
      {patch &&
        discussions.map((thread) => (
          <ThreadPortal key={thread.discussionId} patch={patch} thread={thread} />
        ))}
      <div
        ref={mount}
        className="patch-source patch-rich"
        tabIndex={0}
        aria-label="Selectable unified patch"
        onCopy={(event) => {
          const selection = getSelection();
          if (!selection?.rangeCount || selection.isCollapsed) return;
          const range = selection.getRangeAt(0);
          const text = patch?.copy(range);
          if (text === undefined) return;
          event.clipboardData.setData("text/plain", text);
          event.preventDefault();
        }}
      />
    </>
  );
}

function ThreadPortal({
  patch,
  thread,
}: {
  readonly patch: PatchDocument;
  readonly thread: PatchDiscussion;
}) {
  const [slot, setSlot] = useState<HTMLElement | null>();
  const position = threadLine(thread);
  const side = position?.side;
  const line = position?.number;
  const attach = useCallback(
    (marker: HTMLSpanElement | null) => {
      if (!marker) return;
      setSlot(side !== undefined && line !== undefined ? (patch.anchor(side, line) ?? null) : null);
    },
    [patch, side, line],
  );
  return (
    <>
      <span ref={attach} hidden />
      {slot && createPortal(<ChangesThread thread={thread} />, slot)}
      {slot === null && <ChangesThread thread={thread} outside />}
    </>
  );
}
