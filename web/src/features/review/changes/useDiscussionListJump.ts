import { useLayoutEffect } from "react";
import type { RefObject } from "react";
import type { DiscussionDestination } from "../../../api/generated/contracts";
import { highlightNote, noteMatch } from "./note-highlight";

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
      const top = viewport.getBoundingClientRect().top;
      const toolbar =
        viewport.querySelector(".changes-header")?.getBoundingClientRect().bottom ?? top;
      const before = viewport.scrollTop;
      viewport.scrollTop += rect.top - Math.max(top, toolbar) - 5;
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
