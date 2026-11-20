import { useEffect, useEffectEvent, useLayoutEffect, useRef, useState } from "react";
import { readPatch } from "../../../api/changes";
import type { PatchRange, PatchWindow } from "../../../api/changes";
import type { PatchFile } from "../../../api/generated/contracts";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { sourceReadingTop } from "./patch-position";
import type { SourcePosition } from "./patch-position";

type Move = {
  readonly range: PatchRange;
  readonly byte?: number;
  readonly direction?: "before" | "after";
  readonly entry?: "start" | "end";
  readonly land?: "start" | "end";
};
type Props = {
  readonly file: PatchFile;
  readonly position: SourcePosition | "start" | undefined;
  readonly entrySide: "start" | "end" | undefined;
};

export function PatchSource({ file, position, entrySide }: Props) {
  const pre = useRef<HTMLPreElement>(null);
  const lastScroll = useRef(0);
  const reentry = useRef(entrySide);
  useLayoutEffect(() => {
    if (entrySide) reentry.current = entrySide;
    if (position) reentry.current = undefined;
  }, [entrySide, position]);
  const [pages, setPages] = useState<readonly PatchWindow[]>([]);
  const [move, setMove] = useState<Move | undefined>(() =>
    openingMove(file, position, entrySide === "end"),
  );
  const [previousPosition, setPreviousPosition] = useState(position);
  if (previousPosition !== position) {
    setPreviousPosition(position);
    if (position) setMove(openingMove(file, position, false));
  }
  const [failure, setFailure] = useState<{ readonly move: Move; readonly message: string }>();
  const adjustment = useRef<
    | { readonly range: Range; readonly top: number }
    | { readonly bottom: number }
    | "start"
    | "end"
    | undefined
  >(undefined);
  const head = pages[0];
  const tail = pages.at(-1);
  const finishPage = useEffectEvent((target: Move, loaded: PatchWindow[]) => {
    const page = loaded[0]!;
    try {
      const element = pre.current;
      const scroll = element?.closest<HTMLElement>(".drawer-body");
      const backwards = target.direction === "before";
      if (target.direction && element && scroll && head && tail) {
        if (backwards ? page.next !== head.first : page.first !== tail.next)
          throw new Error("The adjacent patch range changed.");
        const dropped = pages.length === 3 ? (backwards ? tail : head) : undefined;
        const span = dropped && element.querySelector(`[data-source-start="${dropped.first}"]`);
        if (span && heldOrVisible(span, scroll)) {
          setMove(undefined);
          return;
        }
        // The browser keeps the same text nodes and selection while the edge moves.
        // Correct the outer scroll by the actual reflow of a surviving character.
        if (backwards || dropped) {
          const anchor = readingRange(
            element,
            scroll,
            backwards ? head.first : (pages[1] ?? tail).first,
          );
          if (anchor)
            adjustment.current = { range: anchor, top: anchor.getBoundingClientRect().top };
        }
        setPages(backwards ? [page, ...pages.slice(0, 2)] : [...pages.slice(-2), page]);
      } else {
        const boundary = element?.getBoundingClientRect().bottom;
        if (
          target.entry === "end" &&
          element &&
          scroll &&
          boundary !== undefined &&
          boundary > sourceReadingTop(element, scroll) &&
          element.getBoundingClientRect().top < scroll.getBoundingClientRect().bottom
        )
          adjustment.current = { bottom: boundary };
        else
          adjustment.current = element?.closest(".selected-patch")?.contains(document.activeElement)
            ? target.land
            : undefined;
        setPages(loaded);
      }
      setFailure(undefined);
    } catch (error) {
      setFailure({
        move: target,
        message: error instanceof Error ? error.message : "The patch range could not be loaded.",
      });
    }
    setMove(undefined);
  });

  const read = useEffectEvent(async (target: Move, signal: AbortSignal) => {
    const result = [await readPatch(file, signal, target.range)];
    let tail = result[0]!;
    // Restore the same wrapping context for a bookmark inside a giant physical line.
    while (
      target.byte !== undefined &&
      tail.next <= target.byte &&
      tail.next < tail.total &&
      result.length < 3
    ) {
      tail = await readPatch(file, signal, { offset: tail.next, etag: tail.etag });
      result.push(tail);
    }
    return result;
  });
  useEffect(() => {
    if (!move) return;
    const controller = new AbortController();
    void read(move, controller.signal).then(
      (page) => {
        if (!controller.signal.aborted) finishPage(move, page);
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        setFailure({
          move,
          message: error instanceof Error ? error.message : "The patch range could not be loaded.",
        });
        setMove(undefined);
      },
    );
    return () => controller.abort();
  }, [move]);

  useLayoutEffect(() => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    const pending = adjustment.current;
    adjustment.current = undefined;
    if (!element || !scroll || !pending) return;
    if (pending === "end")
      scroll.scrollTop +=
        element.getBoundingClientRect().bottom - scroll.getBoundingClientRect().bottom;
    else if (pending === "start")
      scroll.scrollTop += element.getBoundingClientRect().top - sourceReadingTop(element, scroll);
    else if ("bottom" in pending)
      scroll.scrollTop += element.getBoundingClientRect().bottom - pending.bottom;
    else scroll.scrollTop += pending.range.getBoundingClientRect().top - pending.top;
    lastScroll.current = scroll.scrollTop;
  }, [pages]);

  const hasPages = Boolean(head);
  const approach = useEffectEvent((backwards: boolean) => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    if (!element || !scroll || !head || !tail || move || failure) return;
    const box = element.getBoundingClientRect();
    const view = scroll.getBoundingClientRect();
    const ahead = scroll.clientHeight * 2;
    const side = reentry.current;
    if (side && backwards === (side === "end")) {
      const close = backwards
        ? box.bottom >= view.top - ahead && box.bottom <= view.bottom + ahead
        : box.top <= view.bottom + ahead && box.top >= view.top - ahead;
      if (close && (backwards ? tail.next < tail.total : head.first > 0)) {
        const selection = getSelection();
        if (!selection?.isCollapsed && selection?.containsNode(element, true)) return;
        setMove({
          range: backwards ? { last: true, etag: head.etag } : { offset: 0, etag: head.etag },
          entry: side,
        });
        reentry.current = undefined;
        return;
      }
      if (close) reentry.current = undefined;
    }
    if (box.bottom < view.top || box.top > view.bottom) return;
    if (
      backwards
        ? head.first === 0 || view.top - box.top > ahead
        : tail.next >= tail.total || box.bottom - view.bottom > ahead
    )
      return;
    const dropped = pages.length === 3 ? (backwards ? tail : head) : undefined;
    const span = dropped && element.querySelector(`[data-source-start="${dropped.first}"]`);
    if (span && heldOrVisible(span, scroll)) return;
    setMove(
      backwards
        ? { range: { before: head.first, etag: head.etag }, direction: "before" }
        : { range: { offset: tail.next, etag: tail.etag }, direction: "after" },
    );
  });

  const keepSourceEdge = useEffectEvent(
    (element: HTMLElement, scroll: HTMLElement, previous: number, delta: number) => {
      const backwards = delta < 0;
      if (!head || !tail) return;
      const box = element.getBoundingClientRect();
      const top = sourceReadingTop(element, scroll);
      const bottom = scroll.getBoundingClientRect().bottom;
      const entry = move?.entry ?? failure?.move.entry ?? reentry.current;
      const approachingEntry =
        entry === (backwards ? "end" : "start") &&
        (backwards ? tail.next !== tail.total : head.first !== 0);
      let remaining: number;
      if (approachingEntry) {
        // Stay on the neighboring file until the correct end of this source is ready.
        if (backwards ? box.top + delta >= bottom : box.bottom + delta <= top) return;
        remaining = backwards ? top - (box.bottom + delta) : box.top + delta - bottom;
      } else {
        if (backwards ? head.first === 0 : tail.next === tail.total) return;
        // Constrain only a gesture that started inside this source.
        if (box.bottom + delta <= top || box.top + delta >= bottom) return;
        const retry =
          failure?.move.direction === (backwards ? "before" : "after")
            ? backwards
              ? element.previousElementSibling
              : element.nextElementSibling
            : null;
        const edge = retry?.getBoundingClientRect() ?? box;
        remaining = backwards ? top - (edge.top + delta) : edge.bottom + delta - bottom;
      }
      if (Math.abs(delta) > Math.max(0, remaining))
        scroll.scrollTop = previous + Math.sign(delta) * Math.max(0, remaining);
    },
  );

  useEffect(() => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    if (!element || !scroll) return;
    lastScroll.current = scroll.scrollTop;
    let readerGesture = false;
    let backwards: boolean | undefined;
    const onScroll = () => {
      const delta = scroll.scrollTop - lastScroll.current;
      if (delta && readerGesture) {
        backwards = delta < 0;
        keepSourceEdge(element, scroll, lastScroll.current, delta);
        approach(backwards);
      }
      lastScroll.current = scroll.scrollTop;
    };
    const onSelection = () => {
      if (readerGesture && backwards !== undefined && getSelection()?.isCollapsed)
        approach(backwards);
    };
    const onWheel = (event: WheelEvent) => {
      readerGesture = true;
      if (event.deltaY) {
        backwards = event.deltaY < 0;
        approach(backwards);
      }
    };
    const onTouch = () => {
      readerGesture = true;
    };
    const onPointer = (event: PointerEvent) => {
      readerGesture =
        event.target === scroll || (event.target instanceof Node && element.contains(event.target));
    };
    const onKey = (event: KeyboardEvent) => {
      readerGesture =
        ["ArrowDown", "ArrowUp", "PageDown", "PageUp", "Home", "End", " "].includes(event.key) &&
        !(
          event.target instanceof Element &&
          event.target.closest("button, a, input, summary, select, textarea")
        );
    };
    const signals = new AbortController();
    const options = { passive: true, signal: signals.signal };
    scroll.addEventListener("scroll", onScroll, options);
    scroll.addEventListener("wheel", onWheel, options);
    scroll.addEventListener("touchmove", onTouch, options);
    document.addEventListener("pointerdown", onPointer, options);
    document.addEventListener("keydown", onKey, options);
    document.addEventListener("selectionchange", onSelection, options);
    return () => signals.abort();
  }, [hasPages]);

  const failedAtEnd = failure?.move.direction === "after" || failure?.move.entry === "end";
  const notice = failure && (
    <Notice
      retry={() => {
        setFailure(undefined);
        setMove(failure.move);
      }}
    >
      {failure.message}
    </Notice>
  );

  return (
    <>
      {!head && notice}
      {!head && !failure && (
        <div className="patch-waiting">
          <Loading label="Loading patch…" />
        </div>
      )}
      {head && tail && (
        <>
          <div className="surface-card stack">
            <div className="spread">
              <strong>Large patch · source view</strong>
              <span className="meta">
                Bytes {head.first + 1}–{tail.next} of {tail.total}
              </span>
            </div>
            <div className="actions">
              <button
                type="button"
                onClick={() => setMove({ range: { offset: 0, etag: head.etag }, land: "start" })}
              >
                Start
              </button>
              <button
                type="button"
                onClick={() => setMove({ range: { last: true, etag: head.etag }, land: "end" })}
              >
                End
              </button>
            </div>
            <p className="meta">Browser Find searches the displayed source range.</p>
          </div>
          {!failedAtEnd && notice}
          {head.total === 0 ? (
            <Empty title="This file has an empty patch" />
          ) : (
            <pre
              className="patch-source"
              tabIndex={0}
              ref={pre}
              data-patch-etag={head.etag}
              aria-label="Patch source"
              aria-busy={Boolean(move?.direction)}
              onCopy={(event) => {
                const selection = getSelection();
                if (!selection?.rangeCount || selection.isCollapsed) return;
                const range = selection.getRangeAt(0);
                if (!event.currentTarget.contains(range.commonAncestorContainer)) return;
                event.clipboardData.setData("text/plain", range.toString());
                event.preventDefault();
              }}
            >
              {pages.map((page) => (
                <span key={page.first} data-source-start={page.first}>
                  {page.text}
                </span>
              ))}
            </pre>
          )}
        </>
      )}
      {failedAtEnd && notice}
    </>
  );
}

function openingMove(file: PatchFile, position: Props["position"], entrySide: boolean): Move {
  if (position && position !== "start" && (!file.patchETag || position.etag === file.patchETag))
    return { range: { offset: position.first, etag: position.etag }, byte: position.byte };
  return { range: !position && entrySide ? { last: true } : { offset: 0 } };
}

function heldOrVisible(span: Element, scroll: HTMLElement) {
  const selection = getSelection();
  if (selection && !selection.isCollapsed && selection.containsNode(span, true)) return true;
  const box = span.getBoundingClientRect();
  const view = scroll.getBoundingClientRect();
  return box.bottom > view.top && box.top < view.bottom;
}

function readingRange(pre: HTMLElement, scroll: HTMLElement, fallback: number) {
  const top = sourceReadingTop(pre, scroll);
  const box = pre.getBoundingClientRect();
  if (box.bottom <= top || box.top >= scroll.getBoundingClientRect().bottom) return;
  const point = document.caretPositionFromPoint(box.left + 10, Math.max(box.top + 10, top + 1));
  const range = document.createRange();
  if (point?.offsetNode instanceof Text && pre.contains(point.offsetNode))
    range.setStart(point.offsetNode, point.offset);
  else {
    const node = pre.querySelector(`[data-source-start="${fallback}"]`)?.firstChild;
    if (!node) return;
    range.setStart(node, 0);
  }
  range.collapse(true);
  return range;
}
