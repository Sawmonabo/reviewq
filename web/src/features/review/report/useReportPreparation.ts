import { useEffect, useEffectEvent, useRef, useState } from "react";
import { finishReading } from "./report-position";
import type { ReaderDocument } from "./report-reader";

export function useReportPreparation(
  reader: ReaderDocument,
  wanted: string,
  pinKey: string,
  holdViewport: boolean,
  host: HTMLElement | null,
  fallback: () => void,
) {
  const update = useRef<((value: string) => void) | undefined>(undefined);
  const [, publish] = useState(0);
  const [failure, setFailure] = useState<ReaderDocument>();
  const showFull = useEffectEvent(fallback);
  const pinned = useEffectEvent(() => {
    if (!host) return [];
    const selection = getSelection();
    const range =
      selection?.rangeCount && !selection.isCollapsed ? selection.getRangeAt(0) : undefined;
    // Keep the actual reading surface until an explicitly requested destination
    // commits. The cache exempts only sources it has already prepared.
    const frame = holdViewport ? host.closest(".drawer-body")?.getBoundingClientRect() : undefined;
    return [...host.querySelectorAll<HTMLElement>("[data-report-block]")]
      .filter((row) => {
        if (row.contains(document.activeElement) || range?.intersectsNode(row)) return true;
        if (!frame) return false;
        const box = row.getBoundingClientRect();
        return box.top < frame.bottom && box.bottom > frame.top;
      })
      .map((row) => Number(row.dataset.reportBlock));
  });
  useEffect(() => {
    const prepare = reader.prepare;
    if (!prepare) return;
    let latest: readonly number[] = [];
    let active: AbortController | undefined;
    let preparing: number | undefined;
    let closed = false;
    async function run() {
      while (!closed && latest.length) {
        const plan = latest;
        preparing = undefined;
        const controller = new AbortController();
        active = controller;
        try {
          await prepare!(
            plan,
            controller.signal,
            () => publish((n) => n + 1),
            (index) => {
              if (index !== undefined) preparing = index;
              return plan === latest;
            },
            pinned(),
          );
        } catch {
          if (!controller.signal.aborted && plan === latest) setFailure(reader);
        }
        if (plan === latest) break;
      }
      active = undefined;
    }
    update.current = (value) => {
      latest = value ? value.split(",").map(Number) : [];
      if (!active) void run();
      else if (preparing !== undefined && !latest.includes(preparing)) active.abort();
    };
    return () => {
      closed = true;
      active?.abort();
      update.current = undefined;
    };
  }, [reader]);
  useEffect(() => update.current?.(wanted), [reader, wanted, pinKey, holdViewport]);

  useEffect(() => {
    if (failure !== reader || !host) return;
    const controller = new AbortController();
    void Promise.resolve(finishReading(host, controller.signal)).then(() => {
      if (!controller.signal.aborted) showFull();
    });
    return () => controller.abort();
  }, [failure, reader, host]);
}
