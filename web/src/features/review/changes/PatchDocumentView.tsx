import { useCallback, useState } from "react";
import type { ClipboardEvent } from "react";
import { createPortal } from "react-dom";
import type { PatchDiscussion } from "../../../api/generated/contracts";
import { DiscussionThread } from "../DiscussionThread";
import { mountPatchDocument } from "./patch-document";
import type { PatchDocument } from "./patch-document";
import type { PatchLine } from "./patch-position";

type Props = {
  readonly text: string;
  readonly discussions: readonly PatchDiscussion[];
  readonly priorityLine: PatchLine | undefined;
};

export function PatchDocumentView({ text, discussions, priorityLine }: Props) {
  const [patch, setPatch] = useState<PatchDocument>();
  const [initialLine] = useState(priorityLine);
  const mount = useCallback(
    (host: HTMLDivElement | null) => {
      if (!host) return;
      const rendered = mountPatchDocument(host, text);
      if (initialLine) rendered.line(initialLine.side, initialLine.number);
      setPatch(rendered);
      return rendered.release;
    },
    [text, initialLine],
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
        onCopy={copyRows}
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
  const attach = useCallback(
    (Marker: HTMLSpanElement | null) => {
      if (!Marker) return;
      const anchor = thread.anchor;
      const selected = anchor?.range?.end;
      const endNew = selected?.newLine ?? anchor?.newLine;
      const endOld = selected?.oldLine ?? anchor?.oldLine;
      const target =
        (endNew != null ? patch.anchor(`new:${endNew}`) : undefined) ??
        (endOld != null ? patch.anchor(`old:${endOld}`) : undefined);
      setSlot(target ?? null);
    },
    [patch, thread.anchor],
  );
  return (
    <>
      <span ref={attach} hidden />
      {slot && createPortal(<DiscussionThread thread={thread} />, slot)}
      {slot === null && <DiscussionThread thread={thread} outside />}
    </>
  );
}

function copyRows(event: ClipboardEvent<HTMLDivElement>) {
  const selection = getSelection();
  if (!selection || selection.isCollapsed || !selection.rangeCount) return;
  const range = selection.getRangeAt(0);
  if (!event.currentTarget.contains(range.commonAncestorContainer)) return;
  const startRow = closestRow(range.startContainer);
  const endRow = closestRow(range.endContainer);
  if (!startRow || !endRow) return;
  const chunks: string[] = [];
  const walker = document.createTreeWalker(event.currentTarget, NodeFilter.SHOW_ELEMENT, {
    acceptNode(node) {
      if (!(node instanceof Element)) return NodeFilter.FILTER_SKIP;
      if (node.classList.contains("patch-thread-slot")) return NodeFilter.FILTER_REJECT;
      return node.classList.contains("patch-row")
        ? NodeFilter.FILTER_ACCEPT
        : NodeFilter.FILTER_SKIP;
    },
  });
  walker.currentNode = startRow;
  let row: Node | null = startRow;
  while (row) {
    const child = row.firstChild;
    if (child?.nodeType === Node.TEXT_NODE) {
      const text = child.nodeValue ?? "";
      chunks.push(
        text.slice(
          range.startContainer === child ? range.startOffset : 0,
          range.endContainer === child ? range.endOffset : undefined,
        ),
      );
    }
    if (row === endRow) break;
    row = walker.nextNode();
  }
  if (!chunks.length || row !== endRow) return;
  event.clipboardData.setData("text/plain", chunks.join(""));
  event.preventDefault();
}

function closestRow(node: Node) {
  const element = node instanceof Element ? node : node.parentElement;
  return element?.closest(".patch-row");
}
