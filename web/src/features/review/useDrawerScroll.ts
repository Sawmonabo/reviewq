import { useLayoutEffect, useRef, useState } from "react";
import { savedValue, saveValue } from "../../lib/view-preferences";

export function useDrawerScroll(key: string | undefined) {
  const scroller = useRef<HTMLDivElement>(null);
  const [content, setContent] = useState<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    const host = scroller.current;
    const body = content;
    if (!host || !body || !key) return;
    const storageKey = `scroll.review.${key}`;
    const saved = savedValue(storageKey);
    let pending = typeof saved === "number" && Number.isFinite(saved) && saved > 0 ? saved : 0;
    let offset = pending;
    const observer = new ResizeObserver(restore);

    function restore() {
      if (!host || !pending || host.scrollHeight - host.clientHeight < pending - 1) return;
      host.scrollTop = pending;
      pending = 0;
      observer.disconnect();
    }
    function cancelRestore() {
      if (pending && host) offset = host.scrollTop;
      pending = 0;
      observer.disconnect();
    }
    function record() {
      if (host && !pending) offset = host.scrollTop;
    }
    function keyboard(event: KeyboardEvent) {
      if (["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key))
        cancelRestore();
    }
    function save() {
      saveValue(storageKey, offset);
    }

    if (pending) {
      observer.observe(body);
      restore();
    }
    host.addEventListener("scroll", record, { passive: true });
    host.addEventListener("wheel", cancelRestore, { passive: true });
    host.addEventListener("touchmove", cancelRestore, { passive: true });
    host.addEventListener("keydown", keyboard);
    window.addEventListener("pagehide", save);
    return () => {
      save();
      observer.disconnect();
      host.removeEventListener("scroll", record);
      host.removeEventListener("wheel", cancelRestore);
      host.removeEventListener("touchmove", cancelRestore);
      host.removeEventListener("keydown", keyboard);
      window.removeEventListener("pagehide", save);
    };
  }, [key, content]);
  return { scroller, content: setContent };
}
