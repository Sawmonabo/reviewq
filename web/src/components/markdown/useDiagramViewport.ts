import { useLayoutEffect, useEffectEvent, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

type Size = { readonly width: number; readonly height: number };
type Point = { readonly clientX: number; readonly clientY: number };

const STEP = 0.01;
const MAX_ZOOM = 3;

export function useDiagramViewport(size: Size | undefined) {
  const frame = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLDivElement>(null);
  const automatic = useRef(true);
  const scale = useRef(1);
  const drag = useRef<{ id: number; x: number; y: number; left: number; top: number } | undefined>(
    undefined,
  );
  const [zoom, setZoom] = useState(1);
  const [minimum, setMinimum] = useState(0.1);

  function apply(art: HTMLDivElement, diagram: Size, value: number) {
    art.style.width = `${diagram.width * value}px`;
    scale.current = value;
    setZoom(value);
  }

  function measure() {
    const node = frame.current;
    const art = canvas.current;
    if (!node || !art || !size) return;
    const style = getComputedStyle(node);
    const width = node.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    const height =
      parseFloat(style.maxHeight) - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
    const next = Math.min(MAX_ZOOM, (width - 1) / size.width, (height - 1) / size.height);
    if (!(next > 0)) return;
    setMinimum(Math.min(0.1, next));
    if (automatic.current) {
      apply(art, size, next);
      node.scrollTo(0, 0);
    }
  }

  function fit() {
    automatic.current = true;
    measure();
  }

  function zoomTo(value: number, point?: Point) {
    const node = frame.current;
    const art = canvas.current;
    if (!node || !art || !size) return;
    const next = Math.max(minimum, Math.min(MAX_ZOOM, value));
    automatic.current = false;
    if (next === scale.current) return;
    const bounds = node.getBoundingClientRect();
    const clientX = point?.clientX ?? bounds.left + node.clientWidth / 2;
    const clientY = point?.clientY ?? bounds.top + node.clientHeight / 2;
    const before = art.getBoundingClientRect();
    if (!before.width) return;
    const ratio = (size.width * next) / before.width;
    apply(art, size, next);
    const after = art.getBoundingClientRect();
    node.scrollBy(
      after.left + (clientX - before.left) * ratio - clientX,
      after.top + (clientY - before.top) * ratio - clientY,
    );
  }

  const resize = useEffectEvent(measure);
  const wheel = useEffectEvent((event: WheelEvent) => {
    if (!event.deltaY) return;
    event.preventDefault();
    event.stopPropagation();
    zoomTo(scale.current - Math.sign(event.deltaY) * STEP, event);
  });
  useLayoutEffect(() => {
    const node = frame.current;
    if (!node || !size) return;
    automatic.current = true;
    resize();
    const observer = new ResizeObserver(() => resize());
    observer.observe(node);
    const onResize = () => resize();
    const onWheel = (event: WheelEvent) => wheel(event);
    window.addEventListener("resize", onResize);
    node.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", onResize);
      node.removeEventListener("wheel", onWheel);
    };
  }, [size]);

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "+" || event.key === "=") zoomTo(scale.current + STEP);
    else if (event.key === "-") zoomTo(scale.current - STEP);
    else if (event.key === "0") zoomTo(1);
    else {
      if (
        [
          "ArrowLeft",
          "ArrowRight",
          "ArrowUp",
          "ArrowDown",
          "PageUp",
          "PageDown",
          "Home",
          "End",
        ].includes(event.key)
      )
        automatic.current = false;
      return;
    }
    event.preventDefault();
  }

  function onPointerDown(event: PointerEvent<HTMLDivElement>) {
    if (!size || event.button !== 0 || !event.isPrimary) return;
    automatic.current = false;
    const node = event.currentTarget;
    const bounds = node.getBoundingClientRect();
    if (
      event.clientX - bounds.left >= node.clientWidth ||
      event.clientY - bounds.top >= node.clientHeight
    )
      return;
    event.preventDefault();
    node.focus({ preventScroll: true });
    node.setPointerCapture(event.pointerId);
    drag.current = {
      id: event.pointerId,
      x: event.clientX,
      y: event.clientY,
      left: node.scrollLeft,
      top: node.scrollTop,
    };
  }

  function onPointerMove(event: PointerEvent<HTMLDivElement>) {
    const start = drag.current;
    if (!start || start.id !== event.pointerId) return;
    event.currentTarget.scrollTo(
      start.left + start.x - event.clientX,
      start.top + start.y - event.clientY,
    );
  }

  return {
    frame,
    canvas,
    zoom,
    minimum,
    fit,
    zoomIn: () => zoomTo(scale.current + STEP),
    zoomOut: () => zoomTo(scale.current - STEP),
    reset: () => zoomTo(1),
    gestures: {
      onKeyDown,
      onPointerDown,
      onPointerMove,
      onLostPointerCapture: (event: PointerEvent<HTMLDivElement>) => {
        if (drag.current?.id === event.pointerId) drag.current = undefined;
      },
    },
  };
}
