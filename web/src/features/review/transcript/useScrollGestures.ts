import { useRef } from "react";
import type { TouchEvent, WheelEvent } from "react";

type Point = { readonly clientX: number; readonly clientY: number };

export function useScrollGestures(gesture: (up: boolean, distance: number) => void) {
  const touch = useRef<Point | undefined>(undefined);

  return {
    onWheel(event: WheelEvent<HTMLDivElement>) {
      if (
        event.defaultPrevented ||
        event.ctrlKey ||
        event.metaKey ||
        event.shiftKey ||
        Math.abs(event.deltaY) <= Math.abs(event.deltaX)
      )
        return;
      const unit =
        event.deltaMode === 2 ? event.currentTarget.clientHeight : event.deltaMode === 1 ? 16 : 1;
      gesture(event.deltaY < 0, Math.abs(event.deltaY) * unit);
    },
    onTouchStart(event: TouchEvent<HTMLDivElement>) {
      touch.current = event.touches.length === 1 ? event.touches[0] : undefined;
    },
    onTouchMove(event: TouchEvent<HTMLDivElement>) {
      const previous = touch.current;
      const current = event.touches.length === 1 ? event.touches[0] : undefined;
      touch.current = current;
      if (!previous || !current || event.defaultPrevented) return;
      const vertical = current.clientY - previous.clientY;
      if (Math.abs(vertical) > Math.abs(current.clientX - previous.clientX))
        gesture(vertical > 0, Math.abs(vertical));
    },
  };
}
