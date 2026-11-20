import type { PatchDiscussion, PatchPath } from "../../../api/generated/contracts";
import { savedValue, saveValue } from "../../../lib/view-preferences";

export type PatchLine = {
  readonly side: "new" | "old";
  readonly number: number;
  readonly inset: number;
};
type Bookmark = { readonly fileId: string; readonly inset: number; readonly line?: PatchLine };

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
    return { fileId: value.fileId, inset: Math.max(0, value.inset), ...(line && { line }) };
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

export function alignPatch(
  file: HTMLElement,
  scroll: HTMLElement,
  toolbar: number,
  inset: number,
  line: PatchLine | undefined,
  done: () => void,
) {
  let frame = requestAnimationFrame(align);
  let lastTop: number | undefined;
  let landed = false;
  const observer = new ResizeObserver(schedule);
  const mutation = new MutationObserver(schedule);
  observer.observe(file);
  scroll.addEventListener("scroll", schedule, { passive: true });
  mutation.observe(file, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["data-patch-prepared"],
  });
  const signals = new AbortController();
  for (const event of ["wheel", "touchstart", "pointerdown", "keydown"])
    scroll.addEventListener(event, cancel, { signal: signals.signal, passive: true });

  function schedule() {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(align);
  }
  function align() {
    const top = scroll.getBoundingClientRect().top + toolbar;
    const row = line && file.querySelector<HTMLElement>(`[data-${line.side}="${line.number}"]`);
    const waiting = Boolean(
      line && file.querySelector(".patch-waiting, .patch-rich:not([data-patch-prepared])"),
    );
    if (row && line) {
      const insetNow = row.getBoundingClientRect().top - top;
      const documentTop = scroll.scrollTop + insetNow;
      if (
        lastTop !== undefined &&
        Math.abs(documentTop - lastTop) < 0.5 &&
        Math.abs(insetNow - line.inset) > 1
      ) {
        cancel();
        return;
      }
      lastTop = documentTop;
      if (Math.abs(insetNow - line.inset) > 0.5) scroll.scrollTop += insetNow - line.inset;
      if (!landed) {
        landed = true;
        done();
      }
    } else if (!landed) {
      const offset = Math.min(inset, Math.max(0, file.offsetHeight - 30));
      scroll.scrollTop += file.getBoundingClientRect().top - top + offset;
      if (!waiting) cancel();
    }
  }
  function cancel() {
    release();
    if (!landed) {
      landed = true;
      done();
    }
  }
  function release() {
    cancelAnimationFrame(frame);
    signals.abort();
    scroll.removeEventListener("scroll", schedule);
    mutation.disconnect();
    observer.disconnect();
  }
  return release;
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
