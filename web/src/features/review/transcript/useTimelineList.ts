import { observeElementRect, useVirtualizer } from "@tanstack/react-virtual";
import type { defaultRangeExtractor } from "@tanstack/react-virtual";
import { useCallback, useEffect, useEffectEvent, useRef, useState } from "react";
import type { RefObject } from "react";
import { timelineRowKey, type VisibleRow } from "./timeline-rows";

type Options = {
  readonly viewport: RefObject<HTMLDivElement | null>;
  readonly entries: readonly VisibleRow[];
  readonly rangeExtractor: typeof defaultRangeExtractor;
  readonly anchorToStart: boolean;
  readonly older: boolean;
  readonly newer: boolean;
  readonly readingBelowPreview: (row: VisibleRow) => boolean;
  readonly fillCursor: string | undefined;
  readonly onFill: () => boolean;
};

const MESSAGE_HEIGHT = 95;
const TIMELINE_TOP_INSET = 24;
const FILL_DELAY_MS = 160;
const MAX_FILL_PAGES = 2;

export function useTimelineList({
  viewport,
  entries,
  rangeExtractor,
  anchorToStart,
  older,
  newer,
  readingBelowPreview,
  fillCursor,
  onFill,
}: Options) {
  const [windowHeight, setWindowHeight] = useState(0);
  const filled = useRef(new Set<string>());
  const rowKey = useCallback(
    (index: number) => {
      const row = entries[index];
      return row ? timelineRowKey(row) : index;
    },
    [entries],
  );

  // Keep scroll measurements and the TanStack instance within the transcript.
  // eslint-disable-next-line react-hooks/incompatible-library
  const list = useVirtualizer({
    count: entries.length,
    getScrollElement: () => viewport.current,
    // Height can change without changing the mounted range.
    observeElementRect: (instance, update) =>
      observeElementRect(instance, (rect) => {
        update(rect);
        setWindowHeight(rect.height);
      }),
    getItemKey: rowKey,
    // Resolve retained row keys against the new window before mounting its visible range.
    anchorTo: anchorToStart ? "start" : "end",
    estimateSize: (index) =>
      entries[index]?.kind === "tool-run"
        ? 35
        : entries[index]?.kind === "read-pack"
          ? 55
          : MESSAGE_HEIGHT,
    paddingStart: TIMELINE_TOP_INSET + (older ? 40 : 0),
    paddingEnd: newer ? 40 : 0,
    overscan: 7,
    rangeExtractor,
  });
  list.shouldAdjustScrollPositionOnItemSizeChange = (item, _delta, instance) => {
    const top = instance.scrollOffset ?? 0;
    if (!instance.itemSizeCache.has(item.key)) return item.start < top;
    if (item.end <= top) return true;
    // Keep the middle of the viewport stable if the clipped preview above it grows.
    const middle = top + (instance.scrollRect?.height ?? 0) / 2;
    const row = entries[item.index];
    return row !== undefined && item.start < top && item.end <= middle && readingBelowPreview(row);
  };

  useEffect(() => {
    // Heights belong to the retained pages. Keep measurements for pinned and overlapping rows.
    const retained = new Set(entries.map(timelineRowKey));
    for (const key of list.itemSizeCache.keys()) {
      if (typeof key !== "string" || !retained.has(key)) list.itemSizeCache.delete(key);
    }
  }, [entries, list]);

  const contentHeight = list.getTotalSize();
  const fill = useEffectEvent(() => {
    if (!fillCursor || filled.current.size >= MAX_FILL_PAGES || filled.current.has(fillCursor))
      return;
    const host = viewport.current;
    const first = host?.firstElementChild;
    if (
      host &&
      first &&
      first.getBoundingClientRect().top - host.getBoundingClientRect().top >= MESSAGE_HEIGHT &&
      onFill()
    ) {
      filled.current.add(fillCursor);
    }
  });

  useEffect(() => {
    if (!fillCursor || windowHeight <= contentHeight) return;
    const timer = window.setTimeout(() => fill(), FILL_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [fillCursor, windowHeight, contentHeight]);

  return { list, windowHeight, contentHeight };
}
