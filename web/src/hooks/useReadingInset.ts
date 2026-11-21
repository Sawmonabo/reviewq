import { useCallback, useLayoutEffect, useRef } from "react";
import type { RefObject } from "react";

export function useReadingInset(anchor: RefObject<HTMLElement | null>, page: unknown) {
  const pending = useRef<{ scroll: HTMLElement; inset: number } | undefined>(undefined);
  const remember = useCallback((element: HTMLElement | null) => {
    const scroll = element?.closest<HTMLElement>(".timeline-viewport, .drawer-body");
    if (element && scroll) {
      pending.current = {
        scroll,
        inset: element.getBoundingClientRect().top - scroll.getBoundingClientRect().top,
      };
    }
  }, []);
  useLayoutEffect(() => {
    const element = anchor.current;
    const previous = pending.current;
    pending.current = undefined;
    if (!element || !previous?.scroll.contains(element)) return;
    const inset = element.getBoundingClientRect().top - previous.scroll.getBoundingClientRect().top;
    if (Math.abs(inset - previous.inset) > 0.5) previous.scroll.scrollTop += inset - previous.inset;
  }, [anchor, page]);
  return remember;
}
