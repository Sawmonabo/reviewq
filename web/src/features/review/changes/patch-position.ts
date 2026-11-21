import type { PatchDiscussion, PatchPath } from "../../../api/generated/contracts";
import { savedValue, saveValue } from "../../../lib/view-preferences";

export type SourcePosition = {
  readonly first: number;
  readonly byte: number;
  readonly etag: string;
  readonly inset: number;
};
export type PatchLine = {
  readonly side: "new" | "old";
  readonly number: number;
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
  const bounds = scroll.getBoundingClientRect();
  const top = bounds.top + toolbar;
  for (const element of document.elementsFromPoint(bounds.left + scroll.clientWidth / 2, top + 1)) {
    if (!file.contains(element)) continue;
    const row =
      element.closest<HTMLElement>(".patch-row") ??
      element.closest(".patch-thread-slot")?.previousElementSibling;
    if (!(row instanceof HTMLElement)) continue;
    const side = row.dataset.new === undefined ? "old" : "new";
    const number = Number(row.dataset[side]);
    if (Number.isSafeInteger(number))
      return { side, number, inset: row.getBoundingClientRect().top - top };
  }
}

export type ThreadJump = ({ readonly fileId: string } | { readonly path: PatchPath }) & {
  readonly line: PatchLine;
};

export function threadLine(thread: PatchDiscussion): PatchLine | undefined {
  const anchor = thread.anchor;
  if (thread.outdated || anchor?.positionType !== "text") return;
  const end = anchor.range?.end;
  const next = end?.newLine ?? anchor.newLine;
  const old = end?.oldLine ?? anchor.oldLine;
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
    (thread.anchor.newLine !== null || thread.anchor.oldLine !== null)
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
    return { side: value.side, number: value.number, inset: value.inset };
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
  const pre = file.querySelector<HTMLElement>("pre[data-patch-etag]");
  if (!pre?.dataset.patchEtag) return;
  const top = scroll.getBoundingClientRect().top + toolbar;
  const box = pre.getBoundingClientRect();
  const sample = sourceReadingTop(pre, scroll) + 1;
  if (sample < box.top || sample >= box.bottom) return;
  const position = document.caretPositionFromPoint(box.left + 10, sample);
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
  const byte =
    Number(span.dataset.sourceStart) + new TextEncoder().encode(node.data.slice(0, index)).length;
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
  const pre = file.querySelector<HTMLElement>("pre[data-patch-etag]");
  if (pre?.dataset.patchEtag !== source.etag) return;
  const span = [...pre.querySelectorAll<HTMLElement>("[data-source-start]")]
    .reverse()
    .find((entry) => Number(entry.dataset.sourceStart) <= source.byte);
  const node = span?.firstChild;
  if (!(node instanceof Text)) return;
  let bytes = Number(span?.dataset.sourceStart);
  let index = 0;
  for (const scalar of node.data) {
    if (bytes >= source.byte) break;
    const code = scalar.codePointAt(0)!;
    bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4;
    index += scalar.length;
  }
  if (bytes !== source.byte) return;
  const range = document.createRange();
  range.setStart(node, index);
  range.collapse(true);
  return range.getBoundingClientRect();
}
