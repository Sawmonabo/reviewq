import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { PatchDiscussion, PatchDiscussionNote } from "../../../api/generated/contracts";
import { projectMarkdownText } from "../../../components/markdown/parse-markdown";
import { ChangesThread } from "./ChangesThread";

type CompactNotes = "all" | ReadonlySet<string>;
type ReadingAnchor = { viewport: Element; target: Element | Range; top: number };

const previews = new WeakMap<PatchDiscussionNote, string>();
const INPUT_SETTLE_MS = 120;
const SCROLL_KEYS = new Set([
  "ArrowUp",
  "ArrowDown",
  "PageUp",
  "PageDown",
  "Home",
  "End",
  " ",
  "Tab",
  "Enter",
]);

export function OtherDiscussions({ threads }: { readonly threads: readonly PatchDiscussion[] }) {
  const group = useRef<HTMLDivElement>(null);
  const anchor = useRef<ReadingAnchor | undefined>(undefined);
  const [compact, setCompact] = useState<CompactNotes>("all");

  useEffect(() => {
    const element = group.current;
    const viewport = element?.closest(".drawer-body");
    if (!element || !viewport) return;
    const stream = viewport.querySelector(".file-stream");

    let nearby = false;
    let shown: CompactNotes = "all";
    let frame = 0;
    let settle = 0;
    let held: ReadingAnchor | undefined;
    let streamHeight = 0;
    const fileResize = new ResizeObserver(() => {
      // The file virtualizer may have applied part of the shift already.
      if (!held || !stream) return;
      streamHeight = stream.getBoundingClientRect().height;
      if (!restoreAnchor(held)) release();
    });

    function release() {
      if (!held) return;
      held = undefined;
      viewport?.removeAttribute("data-reading-discussions");
      fileResize.disconnect();
    }

    function readAfterInput() {
      window.clearTimeout(settle);
      settle = window.setTimeout(() => {
        settle = 0;
        schedule();
      }, INPUT_SETTLE_MS);
    }

    function onInput(event: Event) {
      if (!event.isTrusted || (event instanceof KeyboardEvent && !SCROLL_KEYS.has(event.key)))
        return;
      release();
      if (nearby) readAfterInput();
    }

    function onScroll() {
      if (settle) readAfterInput();
      else if (!held && nearby) schedule();
      else if (held && stream?.getBoundingClientRect().height === streamHeight) {
        // A scroll with unchanged file geometry chooses a new reading offset.
        const rect = held.target.getBoundingClientRect();
        const clip = held.viewport.getBoundingClientRect();
        if (rect.bottom <= clip.top || rect.top >= clip.bottom) release();
        else held.top = rect.top - clip.top;
      }
    }

    function onReadingTarget() {
      release();
      schedule();
    }

    function reconcile() {
      frame = 0;
      if (!element || !viewport) return;
      const pageFocused = document.hasFocus();
      const selection = document.getSelection();
      const range =
        selection?.rangeCount && !selection.isCollapsed ? selection.getRangeAt(0) : undefined;
      const selected = range !== undefined && range.intersectsNode(element);
      const focus = document.activeElement;
      const focused = focus !== null && element.contains(focus);
      if (!nearby && (selected || focused)) return;
      if (nearby && !held && !settle && stream) {
        const clip = viewport.getBoundingClientRect();
        const header = viewport.querySelector(".changes-header")?.getBoundingClientRect();
        const readingTop = Math.max(clip.top, header?.bottom ?? clip.top);
        if (
          selected ||
          focused ||
          !pageFocused ||
          element.getBoundingClientRect().top <= readingTop
        ) {
          const target =
            (selected ? selectionPoint(element, range) : undefined) ??
            (focused ? focus : undefined);
          held = target
            ? visibleAnchor(viewport, target, readingTop, clip.bottom)
            : readingAnchor(element, viewport);
          if (held) {
            streamHeight = stream.getBoundingClientRect().height;
            viewport.setAttribute("data-reading-discussions", "");
            fileResize.observe(stream);
          }
        }
      }
      // Native Find can hold a browser-owned match without exposing a DOM selection.
      if (!pageFocused) return;
      const next = nearby
        ? new Set(
            selected
              ? Array.from(element.querySelectorAll<HTMLElement>("[data-discussion-source]"))
                  .filter((note) => range.intersectsNode(note))
                  .map((note) => note.dataset.discussionSource!)
              : [],
          )
        : "all";
      if (sameNotes(shown, next)) return;
      anchor.current = readingAnchor(element, viewport, selected ? range : undefined);
      shown = next;
      setCompact(next);
    }
    // On return from the browser Find toolbar the browser can restore the selection.
    function schedule() {
      if (!frame && (nearby || shown !== "all")) frame = requestAnimationFrame(reconcile);
    }
    const observer = new IntersectionObserver(
      ([match]) => {
        nearby = Boolean(match?.isIntersecting);
        if (!nearby && stream?.getBoundingClientRect().height === streamHeight) release();
        schedule();
      },
      { root: viewport, rootMargin: "640px 0px" },
    );
    observer.observe(element);
    const input = new AbortController();
    const options = { signal: input.signal, passive: true };
    for (const event of ["wheel", "pointerdown", "keydown"])
      viewport.addEventListener(event, onInput, options);
    viewport.addEventListener("scroll", onScroll, options);
    element.addEventListener("focusin", onReadingTarget);
    element.addEventListener("focusout", onReadingTarget);
    document.addEventListener("selectionchange", onReadingTarget);
    window.addEventListener("focus", schedule);
    return () => {
      observer.disconnect();
      fileResize.disconnect();
      release();
      input.abort();
      cancelAnimationFrame(frame);
      window.clearTimeout(settle);
      element.removeEventListener("focusin", onReadingTarget);
      element.removeEventListener("focusout", onReadingTarget);
      document.removeEventListener("selectionchange", onReadingTarget);
      window.removeEventListener("focus", schedule);
    };
  }, []);

  useLayoutEffect(() => {
    const saved = anchor.current;
    anchor.current = undefined;
    if (saved) restoreAnchor(saved);
  }, [compact]);

  return (
    <div className="stack" ref={group}>
      {threads.map((thread) => (
        <div className="changes-conversation" key={thread.discussionId}>
          <ChangesThread
            thread={thread}
            outside={!thread.outdated && Boolean(thread.anchor)}
            previewNote={(note) =>
              compact === "all" || compact.has(note.noteId) ? discussionText(note) : undefined
            }
          />
        </div>
      ))}
    </div>
  );
}

function sameNotes(current: CompactNotes, next: CompactNotes) {
  return (
    current === next ||
    (current !== "all" &&
      next !== "all" &&
      current.size === next.size &&
      [...current].every((id) => next.has(id)))
  );
}

function readingAnchor(
  group: Element,
  viewport: Element,
  range?: Range,
): ReadingAnchor | undefined {
  const clip = viewport.getBoundingClientRect();
  const groupRect = group.getBoundingClientRect();
  if (groupRect.bottom <= clip.top || groupRect.top >= clip.bottom) return;
  const point = range && selectionPoint(group, range);
  const selection = point && visibleAnchor(viewport, point, clip.top, clip.bottom);
  if (selection) return selection;
  const match = Array.from(group.querySelectorAll("summary, article")).find((entry) => {
    const rect = entry.getBoundingClientRect();
    return rect.bottom > clip.top && rect.top < clip.bottom;
  });
  if (match) return { viewport, target: match, top: match.getBoundingClientRect().top - clip.top };
}

function selectionPoint(group: Element, range: Range): Range | undefined {
  const start = group.contains(range.startContainer);
  if (!start && !group.contains(range.endContainer)) return;
  const point = range.cloneRange();
  point.collapse(start);
  return point;
}

function visibleAnchor(
  viewport: Element,
  target: Element | Range,
  top: number,
  bottom: number,
): ReadingAnchor | undefined {
  const rect = target.getBoundingClientRect();
  if (rect.bottom > top && rect.top < bottom)
    return { viewport, target, top: rect.top - viewport.getBoundingClientRect().top };
}

function restoreAnchor({ target, viewport, top }: ReadingAnchor): boolean {
  const owner = target instanceof Range ? target.commonAncestorContainer : target;
  if (!owner.isConnected) return false;
  const rect = target.getBoundingClientRect();
  if (!rect.height) return false;
  const delta = rect.top - viewport.getBoundingClientRect().top - top;
  if (Math.abs(delta) >= 1) viewport.scrollTop += delta;
  return true;
}

function discussionText(note: PatchDiscussionNote): string {
  const retained = previews.get(note);
  if (retained !== undefined) return retained;
  const text = projectMarkdownText(note.body);
  previews.set(note, text);
  return text;
}
