import { useLayoutEffect } from "react";
import type { RefObject } from "react";
import type { DiscussionDestination } from "../../../api/generated/contracts";
import { highlightNote, noteMatch } from "./note-highlight";

export type DiscussionReadingPosition = {
  readonly thread: string;
  readonly note: string | undefined;
  readonly inset: number;
  readonly top: number;
};

export function discussionReadingPosition(
  root: HTMLElement | null,
): DiscussionReadingPosition | undefined {
  const viewport = root?.closest<HTMLElement>(".drawer-body");
  if (!root || !viewport) return;
  const top = readingTop(viewport);
  const bottom = viewport.getBoundingClientRect().bottom;
  for (const thread of root.querySelectorAll<HTMLElement>("[data-discussion-id]")) {
    const id = thread.dataset.discussionId;
    const rect = thread.getBoundingClientRect();
    if (!id || rect.bottom <= top) continue;
    if (rect.top >= bottom) break;
    const note = [...thread.querySelectorAll<HTMLElement>("[data-discussion-note]")].find(
      (item) => {
        const box = item.getBoundingClientRect();
        return box.bottom > top && box.top < bottom;
      },
    );
    return {
      thread: id,
      note: note?.dataset.discussionNote,
      inset: (note?.getBoundingClientRect().top ?? rect.top) - top,
      top: viewport.scrollTop,
    };
  }
}

export function restoreDiscussionReading(
  root: HTMLElement | null,
  position: DiscussionReadingPosition,
) {
  const viewport = root?.closest<HTMLElement>(".drawer-body");
  if (!root || !viewport) return;
  const thread = root.querySelector<HTMLElement>(
    `[data-discussion-id="${CSS.escape(position.thread)}"]`,
  );
  const note = position.note
    ? thread?.querySelector<HTMLElement>(`[data-discussion-note="${CSS.escape(position.note)}"]`)
    : undefined;
  const element = note ?? thread;
  if (!element) return;
  viewport.scrollTop += element.getBoundingClientRect().top - readingTop(viewport) - position.inset;
}

function readingTop(viewport: HTMLElement) {
  const top = viewport.getBoundingClientRect().top;
  const toolbar = viewport.querySelector(".changes-header")?.getBoundingClientRect().bottom ?? top;
  return Math.max(top, toolbar);
}

export function useDiscussionListJump(
  list: RefObject<HTMLElement | null>,
  note: DiscussionDestination | undefined,
) {
  useLayoutEffect(() => {
    const root = list.current;
    const viewport = root?.closest<HTMLElement>(".drawer-body");
    if (!note || !root || !viewport) return;
    let frame = 0;
    let landed = false;
    function schedule() {
      if (!landed && frame === 0) frame = requestAnimationFrame(land);
    }
    function land() {
      frame = 0;
      if (!note || !root || !viewport) return;
      const match = noteMatch(root, note);
      if (!match) return;
      const rect = match.range.getClientRects()[0];
      if (!rect) return;
      const before = viewport.scrollTop;
      viewport.scrollTop += rect.top - readingTop(viewport) - 5;
      if (Math.abs(viewport.scrollTop - before) > 1) {
        schedule();
        return;
      }
      landed = true;
      highlightNote(match.range);
      match.article.focus({ preventScroll: true });
    }
    const mutations = new MutationObserver(schedule);
    mutations.observe(root, { childList: true, subtree: true });
    schedule();
    return () => {
      cancelAnimationFrame(frame);
      mutations.disconnect();
    };
  }, [list, note]);
}
