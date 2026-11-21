import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import type { PatchDiscussion, PatchFile } from "../../../api/generated/contracts";
import { ChangesThread, useExpandedChangesThread } from "./ChangesThread";
import { discussionPreview } from "./discussion-preview";
import { mountPatchDocument } from "./patch-document";
import type { PatchDocument } from "./patch-document";
import { threadLine } from "./patch-position";
import type { PatchRow } from "./patch-rows";
import { createThreadViewport } from "./patch/thread-viewport";

type Props = {
  readonly file: PatchFile;
  readonly text: string;
  readonly discussions: readonly PatchDiscussion[];
  readonly fragments?: readonly PatchRow[] | undefined;
};

export function PatchDocumentView({ file, text, discussions, fragments }: Props) {
  const { oldPath, newPath, created, deleted } = file;
  const [patch, setPatch] = useState<PatchDocument>();
  const [observe] = useState(createThreadViewport);
  const mount = useCallback(
    (host: HTMLDivElement | null) => {
      if (!host) return;
      const rendered = mountPatchDocument(
        host,
        text,
        { oldPath, newPath, created, deleted },
        fragments,
      );
      setPatch(rendered);
      return rendered.release;
    },
    [oldPath, newPath, created, deleted, text, fragments],
  );
  const anchored =
    fragments && patch
      ? discussions.filter((thread) => {
          const line = threadLine(thread);
          return line && patch.hasAnchor(line.side, line.number);
        })
      : discussions;
  return (
    <>
      {patch &&
        anchored.map((thread) => (
          <ThreadPortal key={thread.discussionId} patch={patch} thread={thread} observe={observe} />
        ))}
      <div
        ref={mount}
        className="patch-source patch-rich"
        data-patch-etag={file.patchETag}
        tabIndex={fragments ? undefined : 0}
        aria-label={fragments ? undefined : "Selectable unified patch"}
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
  observe,
}: {
  readonly patch: PatchDocument;
  readonly thread: PatchDiscussion;
  readonly observe: ReturnType<typeof createThreadViewport>;
}) {
  const [slot, setSlot] = useState<HTMLElement | null>();
  const [near, setNear] = useState(false);
  const expanded = useExpandedChangesThread(thread.discussionId);
  useEffect(() => {
    if (slot) return observe(slot, setNear);
  }, [observe, slot]);
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
      {slot &&
        createPortal(
          <ChangesThread
            thread={thread}
            previewNote={near || expanded ? undefined : discussionPreview}
          />,
          slot,
        )}
      {slot === null && <ChangesThread thread={thread} outside />}
    </>
  );
}
