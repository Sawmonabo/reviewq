import { useLayoutEffect, useEffectEvent, useRef, useState } from "react";
import type { KeyboardEvent, PointerEvent } from "react";

type Size = { readonly width: number; readonly height: number };
type Point = { readonly clientX: number; readonly clientY: number };

const STEP = 0.01;
const MAX_ZOOM = 3;
const OVERLAY_SCROLLBAR_PIXELS = 12;

export function useDiagramViewport(size: Size | undefined) {
  const frame = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLDivElement>(null);
  const surface = useRef<HTMLDivElement>(null);
  const extent = useRef<HTMLDivElement>(null);
  const mirrored = useRef({ left: 0, top: 0 });
  const automatic = useRef(true);
  const scale = useRef(1);
  const drag = useRef<{ id: number; x: number; y: number; left: number; top: number } | undefined>(
    undefined,
  );
  const [zoom, setZoom] = useState(1);
  const [minimum, setMinimum] = useState(0.1);
  const [touchPan, setTouchPan] = useState(false);

  function showPan(node: HTMLDivElement, target: HTMLDivElement) {
    const { scrollLeft, scrollTop } = node;
    if (target.scrollLeft !== scrollLeft || target.scrollTop !== scrollTop)
      target.scrollTo(scrollLeft, scrollTop);
    mirrored.current.left = target.scrollLeft;
    mirrored.current.top = target.scrollTop;
  }

  function placeSurface(node: HTMLDivElement) {
    const target = surface.current;
    if (!target) return;
    const { clientWidth, clientHeight, clientLeft, offsetWidth, offsetHeight } = node;
    const { width, height } = node.getBoundingClientRect();
    const scrollbarWidth = offsetWidth - clientWidth;
    const scrollbarHeight = offsetHeight - clientHeight;
    target.style.left = `${clientLeft}px`;
    target.style.width = `${width - scrollbarWidth}px`;
    target.style.height = `${height - scrollbarHeight}px`;
    // Expose the native scrollbar hit targets while the SVG receives events over the drawing.
    const vertical =
      node.scrollHeight > clientHeight ? scrollbarWidth || OVERLAY_SCROLLBAR_PIXELS : 0;
    const horizontal =
      node.scrollWidth > clientWidth ? scrollbarHeight || OVERLAY_SCROLLBAR_PIXELS : 0;
    const x = `${width - vertical}px`;
    const y = `${height - horizontal}px`;
    node.style.clipPath = clientLeft
      ? `polygon(0 0, ${clientLeft}px 0, ${clientLeft}px ${y}, 100% ${y}, 100% 100%, 0 100%)`
      : `polygon(${x} 0, 100% 0, 100% 100%, 0 100%, 0 ${y}, ${x} ${y})`;
    showPan(node, target);
  }

  function apply(art: HTMLDivElement, diagram: Size, value: number) {
    const spacer = extent.current;
    const width = `${diagram.width * value}px`;
    art.style.width = width;
    if (spacer) {
      spacer.style.width = width;
      spacer.style.height = `${diagram.height * value}px`;
    }
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
    placeSurface(node);
  }

  function fit() {
    automatic.current = true;
    measure();
  }

  function zoomTo(value: number, point?: Point) {
    const node = frame.current;
    const art = canvas.current;
    const target = surface.current;
    if (!node || !art || !target || !size) return;
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
    placeSurface(node);
    const after = art.getBoundingClientRect();
    node.scrollBy(
      after.left + (clientX - before.left) * ratio - clientX,
      after.top + (clientY - before.top) * ratio - clientY,
    );
    showPan(node, target);
  }

  const resize = useEffectEvent(measure);
  const wheel = useEffectEvent((event: WheelEvent) => {
    if (!event.deltaY || !(event.ctrlKey || event.metaKey)) return;
    event.preventDefault();
    event.stopPropagation();
    zoomTo(scale.current - Math.sign(event.deltaY) * STEP, event);
  });
  useLayoutEffect(() => {
    const node = frame.current;
    const target = surface.current;
    const viewport = node?.parentElement;
    if (!node || !target || !viewport || !size) return;
    automatic.current = true;
    resize();
    const observer = new ResizeObserver(() => resize());
    observer.observe(node);
    const onResize = () => resize();
    const onWheel = (event: WheelEvent) => wheel(event);
    const onFrameScroll = () => showPan(node, target);
    const onSurfaceScroll = () => {
      const { scrollLeft, scrollTop } = target;
      // Ignore the trailing event from mirroring a native thumb or keyboard scroll.
      if (scrollLeft === mirrored.current.left && scrollTop === mirrored.current.top) return;
      node.scrollTo(scrollLeft, scrollTop);
      showPan(node, target);
    };
    window.addEventListener("resize", onResize);
    node.addEventListener("scroll", onFrameScroll);
    target.addEventListener("scroll", onSurfaceScroll);
    viewport.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", onResize);
      node.removeEventListener("scroll", onFrameScroll);
      target.removeEventListener("scroll", onSurfaceScroll);
      viewport.removeEventListener("wheel", onWheel);
      drag.current = undefined;
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
    const node = frame.current;
    if (
      !node ||
      event.button !== 0 ||
      !event.isPrimary ||
      (event.pointerType === "touch" && !touchPan)
    )
      return;
    automatic.current = false;
    event.preventDefault();
    node.focus({ preventScroll: true });
    event.currentTarget.setPointerCapture(event.pointerId);
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
    const node = frame.current;
    if (!start || start.id !== event.pointerId || !node) return;
    node.scrollTo(start.left + start.x - event.clientX, start.top + start.y - event.clientY);
    showPan(node, event.currentTarget);
  }

  return {
    frame,
    canvas,
    surface,
    extent,
    zoom,
    minimum,
    touchPan,
    toggleTouchPan: () => setTouchPan((active) => !active),
    fit,
    zoomIn: () => zoomTo(scale.current + STEP),
    zoomOut: () => zoomTo(scale.current - STEP),
    reset: () => zoomTo(1),
    onKeyDown,
    gestures: {
      onPointerDown,
      onPointerMove,
      onLostPointerCapture: (event: PointerEvent<HTMLDivElement>) => {
        if (drag.current?.id === event.pointerId) drag.current = undefined;
      },
    },
  };
}
