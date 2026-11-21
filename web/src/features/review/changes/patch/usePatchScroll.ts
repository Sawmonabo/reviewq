import { useLayoutEffect, useMemo, useRef } from "react";
import type { RefObject } from "react";
import { sourceAt, sourceRect } from "../patch-position";
import type { SourcePosition } from "../patch-position";

type Reading = {
  readonly file: HTMLElement;
  readonly position: SourcePosition;
  readonly inset: number;
};

export type PatchScroll = ReturnType<typeof usePatchScroll>;

export function usePatchScroll(
  host: HTMLElement | null,
  scroller: HTMLElement | null,
  toolbar: RefObject<HTMLDivElement | null>,
) {
  const active = useRef<{
    readonly preserve: () => void;
    readonly reconcile: () => void;
    readonly move: (offset: number) => void;
    readonly moved: (offset: number) => boolean;
  } | null>(null);
  useLayoutEffect(() => {
    if (!host || !scroller) return;
    const scroll = scroller;
    const controls = toolbar.current;
    const signals = new AbortController();
    const options = { signal: signals.signal, passive: true };
    let reading: Reading | undefined;
    let observed: HTMLElement | undefined;
    let programmatic: number | undefined;
    let frame = 0;
    const resize = new ResizeObserver(reconcile);
    resize.observe(host);
    resize.observe(scroll);
    if (controls) resize.observe(controls);

    function readingTop(clip: DOMRect) {
      return Math.max(clip.top, controls?.getBoundingClientRect().bottom ?? clip.top);
    }

    function anchored() {
      return Boolean(reading?.file.isConnected) && !scroll.hasAttribute("data-reading-discussions");
    }

    function capture() {
      reading = undefined;
      const clip = scroll.getBoundingClientRect();
      const top = readingTop(clip);
      for (const inset of [0, 12, 30]) {
        const y = top + inset;
        if (y + 1 >= clip.bottom) break;
        const file = document
          .elementFromPoint(clip.left + scroll.clientWidth / 2, y + 1)
          ?.closest<HTMLElement>("[data-file-position]");
        if (!file || !host?.contains(file)) continue;
        const position = sourceAt(file, scroll, y - clip.top);
        if (!position) continue;
        reading = { file, position, inset: position.inset + inset };
        if (observed !== file) {
          if (observed) resize.unobserve(observed);
          observed = file;
          resize.observe(file);
        }
        break;
      }
    }

    function restore() {
      if (!reading || !anchored()) return;
      const rect = sourceRect(reading.file, reading.position);
      if (!rect?.height) return;
      const delta = rect.top - readingTop(scroll.getBoundingClientRect()) - reading.inset;
      if (Math.abs(delta) > 0.5) {
        scroll.scrollTop += delta;
        programmatic = scroll.scrollTop;
      }
    }

    function reconcile() {
      if (!reading) capture();
      restore();
      cancelAnimationFrame(frame);
      // File extents also update through the outer virtualizer's measurement.
      frame = requestAnimationFrame(restore);
    }

    function move(offset: number) {
      reading = undefined;
      scroll.scrollTop = offset;
      programmatic = scroll.scrollTop;
      capture();
    }

    function moved(offset: number) {
      return programmatic !== undefined && Math.abs(offset - programmatic) < 0.75;
    }

    function onScroll() {
      if (moved(scroll.scrollTop)) return;
      programmatic = undefined;
      capture();
    }

    scroll.addEventListener("scroll", onScroll, options);
    document.fonts.addEventListener("loadingdone", reconcile, options);
    capture();
    active.current = {
      preserve: () => {
        if (!reading) capture();
      },
      reconcile,
      move,
      moved,
    };
    return () => {
      signals.abort();
      resize.disconnect();
      cancelAnimationFrame(frame);
      active.current = null;
    };
  }, [host, scroller, toolbar]);
  return useMemo(
    () => ({
      preserve: () => active.current?.preserve(),
      reconcile: () => active.current?.reconcile(),
      move: (offset: number) => active.current?.move(offset),
      moved: (offset: number) => active.current?.moved(offset) ?? false,
    }),
    [],
  );
}
