import { useRef } from "react";
import type { RefObject } from "react";

const INITIAL = 50;

export function Splitter({
  host,
  value,
  onChange,
}: {
  readonly host: RefObject<HTMLDivElement | null>;
  readonly value: number;
  readonly onChange: (value: number) => void;
}) {
  const drag = useRef<{ x: number; value: number } | null>(null);

  function adjust(start: number, delta: number) {
    const panel = host.current;
    if (!panel) return;
    const width = panel.clientWidth - Number.parseFloat(getComputedStyle(panel).paddingInlineEnd);
    if (width > 0) onChange(clampSplit(start + (delta * 100) / width, width));
  }
  return (
    <div
      className="splitter"
      role="separator"
      tabIndex={0}
      aria-label="Resize review pane"
      aria-orientation="vertical"
      aria-valuemin={24}
      aria-valuemax={76}
      aria-valuenow={Math.round(value)}
      title="Drag to resize · Arrow keys adjust · Double-click restores 50/50"
      onDoubleClick={() => onChange(INITIAL)}
      onPointerDown={(event) => {
        if (event.button !== 0 || !event.isPrimary) return;
        event.preventDefault();
        drag.current = { x: event.clientX, value };
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={(event) => {
        if (drag.current && event.currentTarget.hasPointerCapture(event.pointerId)) {
          adjust(drag.current.value, event.clientX - drag.current.x);
        }
      }}
      onLostPointerCapture={() => {
        drag.current = null;
      }}
      onKeyDown={(event) => {
        const step = event.shiftKey ? 50 : 20;
        if (event.key === "ArrowLeft") adjust(value, -step);
        else if (event.key === "ArrowRight") adjust(value, step);
        else if (event.key === "Home") adjust(value, -10_000);
        else if (event.key === "End") adjust(value, 10_000);
        else return;
        event.preventDefault();
      }}
    />
  );
}

export function clampSplit(value: number, width: number) {
  const minimum = Math.max(24, Math.min(49, (265 / width) * 100));
  return Math.max(minimum, Math.min(100 - minimum, value));
}
