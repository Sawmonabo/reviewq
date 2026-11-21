import type { PatchDiscussion, PatchPath } from "../../../api/generated/contracts";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { patchDocument } from "./patch-document";
import type { PatchDocument } from "./patch-document";
import { indexAtByte, utf8Length } from "./patch-rows";

export type SourcePosition = {
  readonly first: number;
  readonly byte: number;
  readonly end?: number | undefined;
  readonly etag: string;
  readonly inset: number;
};
export type PatchLine = {
  readonly side: "new" | "old";
  readonly number: number;
  readonly column?: number | undefined;
  readonly inset: number;
};
type Bookmark = {
  readonly fileId: string;
  readonly inset: number;
  readonly line?: PatchLine;
  readonly source?: SourcePosition;
};

export function storedFile(reviewId: string): string | undefined {
  const value = savedValue(`changes.file.${reviewId}`);
  return typeof value === "string" ? value : undefined;
}

export function storedPatch(reviewId: string, revision: string): Bookmark | undefined {
  const value = savedValue(`changes.position.${reviewId}`);
  if (
    value &&
    typeof value === "object" &&
    "revision" in value &&
    value.revision === revision &&
    "fileId" in value &&
    typeof value.fileId === "string" &&
    "inset" in value &&
    typeof value.inset === "number" &&
    Number.isFinite(value.inset)
  ) {
    const line = "line" in value ? readLine(value.line) : undefined;
    const source = "source" in value ? readSource(value.source) : undefined;
    return {
      fileId: value.fileId,
      inset: Math.max(0, value.inset),
      ...(line && { line }),
      ...(source && { source }),
    };
  }
  const fileId = storedFile(reviewId);
  if (!fileId) return;
  const legacy = savedValue(`patch.scroll.${reviewId}.${revision}.${fileId}`);
  if (
    typeof legacy !== "object" ||
    !legacy ||
    !("line" in legacy) ||
    typeof legacy.line !== "string"
  )
    return;
  const [side, number] = legacy.line.split(":");
  const line = readLine({
    side,
    number: Number(number),
    inset: "inset" in legacy ? legacy.inset : 0,
  });
  if (line) return { fileId, inset: 0, line };
}

export function storePatch(reviewId: string, revision: string, position: Bookmark) {
  saveValue(`changes.file.${reviewId}`, position.fileId);
  saveValue(`changes.position.${reviewId}`, { revision, ...position });
}

export function lineAt(
  file: HTMLElement,
  scroll: HTMLElement,
  toolbar: number,
): PatchLine | undefined {
  const top = scroll.getBoundingClientRect().top + toolbar;
  for (const rich of file.querySelectorAll<HTMLElement>(".patch-rich")) {
    const line = patchDocument(rich)?.readingLine(top);
    if (line) return line;
  }
}

export function patchLineTarget(file: HTMLElement, line: PatchLine) {
  for (const rich of file.querySelectorAll<HTMLElement>(".patch-rich")) {
    const target = patchDocument(rich)?.line(line);
    if (target) return target;
  }
}

export function patchMatchRanges(file: HTMLElement, match: Parameters<PatchDocument["match"]>[0]) {
  const ranges: Range[] = [];
  for (const rich of file.querySelectorAll<HTMLElement>(".patch-rich")) {
    const range = patchDocument(rich)?.match(match);
    if (range) ranges.push(range);
  }
  return ranges;
}

export type ThreadJump = (
  | { readonly fileId: string }
  | { readonly path: PatchPath }
  | { readonly paths: Pick<PatchPath, "oldPath" | "newPath"> }
) & { readonly line: PatchLine; readonly thread: PatchDiscussion };

export function threadLine(thread: PatchDiscussion): PatchLine | undefined {
  const anchor = thread.anchor;
  if (thread.outdated || anchor?.positionType !== "text") return;
  const end = anchor.range?.end ?? anchor;
  const next = end.newLine;
  const old = end.oldLine;
  if (next != null) return { side: "new", number: next, inset: 0 };
  if (old != null) return { side: "old", number: old, inset: 0 };
}

export function matchingFile(
  thread: PatchDiscussion,
  file: Pick<PatchPath, "oldPath" | "newPath">,
) {
  return (
    !thread.outdated &&
    thread.anchor !== null &&
    thread.anchor.positionType === "text" &&
    thread.anchor.oldPath === file.oldPath &&
    thread.anchor.newPath === file.newPath &&
    threadLine(thread) !== undefined
  );
}

function readLine(value: unknown): PatchLine | undefined {
  if (
    value &&
    typeof value === "object" &&
    "side" in value &&
    (value.side === "new" || value.side === "old") &&
    "number" in value &&
    typeof value.number === "number" &&
    Number.isSafeInteger(value.number) &&
    value.number >= 0 &&
    "inset" in value &&
    typeof value.inset === "number" &&
    Number.isFinite(value.inset)
  )
    return {
      side: value.side,
      number: value.number,
      inset: value.inset,
      column:
        "column" in value &&
        typeof value.column === "number" &&
        Number.isSafeInteger(value.column) &&
        value.column >= 0
          ? value.column
          : undefined,
    };
}

function readSource(value: unknown): SourcePosition | undefined {
  if (
    value &&
    typeof value === "object" &&
    "etag" in value &&
    typeof value.etag === "string" &&
    /^"[0-9a-f]{64}"$/.test(value.etag) &&
    "first" in value &&
    typeof value.first === "number" &&
    Number.isSafeInteger(value.first) &&
    value.first >= 0 &&
    "byte" in value &&
    typeof value.byte === "number" &&
    Number.isSafeInteger(value.byte) &&
    value.byte >= value.first &&
    "inset" in value &&
    typeof value.inset === "number" &&
    Number.isFinite(value.inset)
  )
    return { first: value.first, byte: value.byte, etag: value.etag, inset: value.inset };
}

export function sourceAt(
  file: HTMLElement,
  scroll: HTMLElement,
  toolbar: number,
): SourcePosition | undefined {
  const top = scroll.getBoundingClientRect().top + toolbar;
  for (const rich of file.querySelectorAll<HTMLElement>(".patch-rich[data-patch-etag]")) {
    const page = rich.closest<HTMLElement>("[data-source-start]");
    if (!page || !rich.dataset.patchEtag) continue;
    const reading = patchDocument(rich)?.readingSource(top);
    if (reading)
      return { ...reading, first: Number(page.dataset.sourceStart), etag: rich.dataset.patchEtag };
  }
  const pre = file.querySelector<HTMLElement>("pre[data-patch-etag]");
  if (!pre?.dataset.patchEtag) return;
  const box = pre.getBoundingClientRect();
  const sample = sourceReadingTop(pre, scroll) + 1;
  if (sample < box.top || sample >= box.bottom) return;
  const position = document.caretPositionFromPoint?.(box.left + 10, sample);
  const node = position?.offsetNode;
  const span = node?.parentElement;
  if (
    !position ||
    !(node instanceof Text) ||
    !span?.hasAttribute("data-source-start") ||
    !pre.contains(span)
  )
    return;
  const index = position.offset;
  const range = document.createRange();
  range.setStart(node, index);
  range.collapse(true);
  const first = Number(pre.querySelector<HTMLElement>("[data-source-start]")?.dataset.sourceStart);
  const byte = Number(span.dataset.sourceStart) + utf8Length(node.data.slice(0, index));
  return {
    first,
    byte,
    etag: pre.dataset.patchEtag,
    inset: range.getBoundingClientRect().top - top,
  };
}

export function sourceReadingTop(pre: HTMLElement, scroll: HTMLElement) {
  const controls = pre
    .closest(".file-stream")
    ?.closest(".changes")
    ?.querySelector(".changes-header");
  return Math.max(
    scroll.getBoundingClientRect().top,
    controls?.getBoundingClientRect().bottom ?? 0,
  );
}

export function sourceRect(file: HTMLElement, source: SourcePosition) {
  for (const rich of file.querySelectorAll<HTMLElement>(".patch-rich[data-patch-etag]")) {
    if (rich.dataset.patchEtag !== source.etag) continue;
    const range = patchDocument(rich)?.sourceRange(source.byte);
    if (range) return range.getBoundingClientRect();
  }
  const range = sourceRanges(file, source, source.byte)[0];
  return range?.getBoundingClientRect();
}

export function sourceRanges(file: HTMLElement, source: SourcePosition, end: number) {
  const pre = file.querySelector<HTMLElement>("pre[data-patch-etag]");
  if (pre?.dataset.patchEtag !== source.etag) return [];
  const ranges: Range[] = [];
  let byte = source.byte;
  for (const span of pre.querySelectorAll<HTMLElement>("[data-source-start]")) {
    const node = span.firstChild;
    if (!(node instanceof Text)) continue;
    const first = Number(span.dataset.sourceStart);
    const next = first + utf8Length(node.data);
    if (byte < first || byte > next || (byte === next && byte < end)) continue;
    const from = indexAtByte(node.data, byte - first);
    const until = Math.min(next, end);
    const to = indexAtByte(node.data, until - first);
    if (from === undefined || to === undefined) return [];
    const range = new Range();
    range.setStart(node, from);
    range.setEnd(node, to);
    ranges.push(range);
    if (until === end) return ranges;
    byte = until;
  }
  return [];
}
