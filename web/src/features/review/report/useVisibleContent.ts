import { useEffect, useEffectEvent } from "react";
import type { RefObject } from "react";

export function useVisibleContent(
  ref: RefObject<HTMLElement | null>,
  identity: string | undefined,
  onVisible: (identity: string) => void,
) {
  const notify = useEffectEvent(onVisible);

  useEffect(() => {
    const node = ref.current;
    if (!node || !identity) return;
    let visible = false;
    let finished = false;
    function report() {
      if (finished || !identity || !visible || document.visibilityState !== "visible") return;
      finished = true;
      notify(identity);
      observer.disconnect();
    }
    const observer = new IntersectionObserver(
      ([entry]) => {
        visible = Boolean(entry?.isIntersecting && entry.intersectionRect.height > 0);
        report();
      },
      { root: node.closest(".drawer-body") },
    );
    observer.observe(node);
    document.addEventListener("visibilitychange", report);
    return () => {
      finished = true;
      observer.disconnect();
      document.removeEventListener("visibilitychange", report);
    };
  }, [identity, ref]);
}
