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
  function adjust(delta: number) {
    const width = host.current?.getBoundingClientRect().width;
    if (width) onChange(clampSplit(value + (delta * 100) / width, width));
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
      onDoubleClick={() => onChange(INITIAL)}
      onPointerDown={(event) => {
        if (event.button === 0) event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={(event) => {
        if (!event.currentTarget.hasPointerCapture(event.pointerId)) return;
        const bounds = host.current?.getBoundingClientRect();
        if (bounds)
          onChange(clampSplit(((event.clientX - bounds.left) * 100) / bounds.width, bounds.width));
      }}
      onKeyDown={(event) => {
        const step = event.shiftKey ? 50 : 20;
        if (event.key === "ArrowLeft") adjust(-step);
        else if (event.key === "ArrowRight") adjust(step);
        else if (event.key === "Home") adjust(-10_000);
        else if (event.key === "End") adjust(10_000);
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
