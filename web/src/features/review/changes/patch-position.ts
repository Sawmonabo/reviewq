import { savedValue, saveValue } from "../../../lib/view-preferences";
import type { PatchDocument } from "./patch-document";

type Position = { readonly top: number; readonly line?: string; readonly inset?: number };

function readPosition(key: string): Position {
  const value = savedValue(key);
  if (typeof value === "number" && Number.isFinite(value)) return { top: Math.max(0, value) };
  if (typeof value !== "object" || !value || !("top" in value)) return { top: 0 };
  const { top } = value;
  if (typeof top !== "number" || !Number.isFinite(top) || top < 0) return { top: 0 };
  if (
    "line" in value &&
    typeof value.line === "string" &&
    /^(new|old):\d+$/.test(value.line) &&
    "inset" in value &&
    typeof value.inset === "number" &&
    Number.isFinite(value.inset)
  ) {
    return { top, line: value.line, inset: value.inset };
  }
  return { top };
}

function positionAt(patch: PatchDocument, scroller: HTMLElement): Position {
  const top = scroller.scrollTop;
  const bounds = scroller.getBoundingClientRect();
  const point = document.elementsFromPoint(bounds.left + scroller.clientWidth / 2, bounds.top + 1);
  for (const element of point) {
    if (element.parentElement !== patch.host) continue;
    let row: Element | null = element;
    if (row.classList.contains("patch-thread-slot")) row = row.previousElementSibling;
    while (row && !row.hasAttribute("data-line")) row = row.nextElementSibling;
    const line = row?.getAttribute("data-line");
    if (line && row) return { top, line, inset: row.getBoundingClientRect().top - bounds.top };
    break;
  }
  return { top };
}

export function bindPatchPosition(patch: PatchDocument, scroller: HTMLElement, key: string) {
  const saved = readPosition(key);
  const row = saved.line ? patch.line(saved.line) : undefined;
  const inset = saved.inset ?? 0;
  let contentTop: number | undefined;
  let restoring = Boolean(row);
  const observer = new ResizeObserver(align);
  const events = new AbortController();
  const options = { signal: events.signal, passive: true } as const;
  function release() {
    restoring = false;
    observer.disconnect();
  }

  function align() {
    if (!restoring || !row) return;
    const rowInset = row.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
    const nextTop = scroller.scrollTop + rowInset;
    const distance = rowInset - inset;
    // Find and focus can scroll without pointer or key events in this pane.
    // A stationary row in the document means the reader moved independently.
    if (
      contentTop !== undefined &&
      Math.abs(contentTop - nextTop) <= 0.5 &&
      Math.abs(distance) > 1
    ) {
      release();
      return;
    }
    contentTop = nextTop;
    if (Math.abs(distance) > 0.5) scroller.scrollTop += distance;
  }

  scroller.scrollTop = saved.top;
  align();
  if (row) {
    // Portals above the reading position can finish after the patch text mounts.
    observer.observe(patch.host);
    const panel = patch.host.closest(".selected-patch");
    if (panel) observer.observe(panel);
    for (const event of ["wheel", "touchstart", "pointerdown", "keydown"]) {
      scroller.addEventListener(event, release, options);
    }
  }

  let last = saved;
  const capture = () => {
    if (!patch.host.isConnected || scroller.clientHeight === 0) return;
    align();
    last = positionAt(patch, scroller);
  };
  const persist = () => saveValue(key, last);
  scroller.addEventListener("scroll", capture, options);
  scroller.addEventListener("scrollend", persist, options);
  window.addEventListener("pagehide", persist, options);
  return () => {
    events.abort();
    release();
    persist();
  };
}
