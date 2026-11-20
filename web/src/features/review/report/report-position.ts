import { savedValue } from "../../../lib/view-preferences";
import type { ReaderDocument } from "./report-reader";

type Bookmark = {
  readonly byte?: number;
  readonly top: number;
  readonly offset?: number;
  readonly inset?: number;
};
export function restoreBookmark(
  host: HTMLElement,
  scroll: HTMLElement,
  position: Bookmark,
  index: number,
  release: () => void,
) {
  let frame = 0;
  let lastAnchor: number | undefined;
  let lastTop: number | undefined;
  const signals = new AbortController();
  const observer = new ResizeObserver(schedule);
  const mutation = new MutationObserver(schedule);
  observer.observe(host);
  observer.observe(scroll);
  mutation.observe(host, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["style"],
  });
  for (const name of ["wheel", "touchstart", "pointerdown", "keydown"])
    scroll.addEventListener(name, release, { signal: signals.signal, passive: true });
  scroll.addEventListener("scroll", schedule, { signal: signals.signal, passive: true });
  schedule();

  function schedule() {
    if (!frame) frame = requestAnimationFrame(align);
  }
  function align() {
    frame = 0;
    const row =
      index >= 0 ? host.querySelector<HTMLElement>(`[data-report-block="${index}"]`) : undefined;
    if (!row || position.inset === undefined) {
      scroll.scrollTop = position.top;
      release();
      return;
    }
    const inset = row.getBoundingClientRect().top - scroll.getBoundingClientRect().top;
    const anchor = scroll.scrollTop + inset;
    // A wheel, keyboard, or independent programmatic scroll ends the restore.
    if (
      lastAnchor !== undefined &&
      Math.abs(anchor - lastAnchor) < 0.5 &&
      lastTop !== undefined &&
      Math.abs(scroll.scrollTop - lastTop) > 1
    ) {
      release();
      return;
    }
    if (Math.abs(inset - position.inset) > 0.5) scroll.scrollTop += inset - position.inset;
    lastTop = scroll.scrollTop;
    lastAnchor = anchor;
  }
  return () => {
    cancelAnimationFrame(frame);
    signals.abort();
    observer.disconnect();
    mutation.disconnect();
  };
}

export function finishReading(element: Element | null, signal: AbortSignal) {
  if (!element) return;
  const host = element;
  function held() {
    const selection = getSelection();
    return (
      (host.contains(document.activeElement) &&
        !document.activeElement?.matches(".report-section-nav button[aria-busy=true]")) ||
      Boolean(
        selection?.rangeCount &&
        !selection.isCollapsed &&
        selection.getRangeAt(0).intersectsNode(host),
      )
    );
  }
  if (!held()) return;
  return new Promise<void>((resolve) => {
    const events = new AbortController();
    function check() {
      if (!signal.aborted && host.isConnected && held()) return;
      events.abort();
      signal.removeEventListener("abort", check);
      resolve();
    }
    signal.addEventListener("abort", check, { once: true });
    document.addEventListener("selectionchange", check, { signal: events.signal });
    document.addEventListener("focusout", () => queueMicrotask(check), { signal: events.signal });
    check();
  });
}

export function bookmarkAt(
  host: HTMLElement,
  scroll: HTMLElement,
  reader: ReaderDocument,
): Bookmark {
  const top = scroll.scrollTop;
  const frame = scroll.getBoundingClientRect();
  for (const row of host.querySelectorAll<HTMLElement>("[data-report-offset]")) {
    const bounds = row.getBoundingClientRect();
    const offset = Number(row.dataset.reportOffset);
    if (
      bounds.bottom > frame.top &&
      bounds.top < frame.bottom &&
      Number.isSafeInteger(offset) &&
      offset >= 0
    ) {
      const byte = reader.byteAt?.(Number(row.dataset.reportBlock));
      return { top, offset, inset: bounds.top - frame.top, ...(byte !== undefined && { byte }) };
    }
  }
  return { top };
}

export function readBookmark(key: string): Bookmark | undefined {
  const item = savedValue(key);
  if (
    !item ||
    typeof item !== "object" ||
    !("top" in item) ||
    typeof item.top !== "number" ||
    !Number.isFinite(item.top) ||
    item.top < 0
  )
    return;
  if (
    "offset" in item &&
    typeof item.offset === "number" &&
    Number.isSafeInteger(item.offset) &&
    item.offset >= 0 &&
    "inset" in item &&
    typeof item.inset === "number" &&
    Number.isFinite(item.inset)
  ) {
    return {
      top: item.top,
      offset: item.offset,
      inset: item.inset,
      ...("byte" in item &&
        typeof item.byte === "number" &&
        Number.isSafeInteger(item.byte) &&
        item.byte >= 0 && { byte: item.byte }),
    };
  }
  return { top: item.top };
}
