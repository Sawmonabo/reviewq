import { useCallback, useState } from "react";
import { createPortal } from "react-dom";
import type { PatchDiscussion } from "../../../api/generated/contracts";
import { ChangesThread } from "./ChangesThread";
import { mountPatchDocument } from "./patch-document";
import type { PatchDocument } from "./patch-document";

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
  const end = thread.anchor?.range?.end;
  const endNew = end?.newLine ?? thread.anchor?.newLine;
  const endOld = end?.oldLine ?? thread.anchor?.oldLine;
  const attach = useCallback(
    (marker: HTMLSpanElement | null) => {
      if (!marker) return;
      const target =
        (endNew != null ? patch.anchor("new", endNew) : undefined) ??
        (endOld != null ? patch.anchor("old", endOld) : undefined);
      setSlot(target ?? null);
    },
    [patch, endNew, endOld],
  );
  return (
    <>
      <span ref={attach} hidden />
      {slot && createPortal(<ChangesThread thread={thread} />, slot)}
      {slot === null && <ChangesThread thread={thread} outside />}
    </>
  );
}
