import { useEffect, useEffectEvent } from "react";
import type { RefObject } from "react";

export function useVisibleContent(
  ref: RefObject<HTMLElement | null>,
  identity: string | undefined,
  onVisible: (identity: string) => void,
  selector?: string,
) {
  const notify = useEffectEvent(onVisible);

  useEffect(() => {
    const node = ref.current;
    if (!node || !identity) return;
    const visible = new Set<Element>();
    const observed = new Set<Element>();
    let finished = false;
    function report() {
      if (finished || !identity || !visible.size || document.visibilityState !== "visible") return;
      finished = true;
      notify(identity);
      observer.disconnect();
      mutation?.disconnect();
    }
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.target.isConnected && entry.isIntersecting && entry.intersectionRect.height > 0)
            visible.add(entry.target);
          else visible.delete(entry.target);
        }
        report();
      },
      { root: node.closest(".drawer-body") },
    );
    function synchronize() {
      for (const child of observed)
        if (!child.isConnected) {
          observer.unobserve(child);
          observed.delete(child);
          visible.delete(child);
        }
      for (const child of selector ? node!.querySelectorAll(selector) : [node!])
        if (!observed.has(child)) {
          observer.observe(child);
          observed.add(child);
        }
    }
    const mutation = selector ? new MutationObserver(synchronize) : undefined;
    mutation?.observe(node, { childList: true, subtree: true });
    synchronize();
    document.addEventListener("visibilitychange", report);
    return () => {
      finished = true;
      observer.disconnect();
      mutation?.disconnect();
      document.removeEventListener("visibilitychange", report);
    };
  }, [identity, ref, selector]);
}
