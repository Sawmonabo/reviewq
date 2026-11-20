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
  readonly land?: "start" | "end";
};
type Props = {
  readonly file: PatchFile;
  readonly position: SourcePosition | "start" | undefined;
  readonly enterFromEnd: boolean;
};

export function PatchSource({ file, position, enterFromEnd }: Props) {
  const pre = useRef<HTMLPreElement>(null);
  const [pages, setPages] = useState<readonly PatchWindow[]>([]);
  const [move, setMove] = useState<Move | undefined>(() =>
    openingMove(file, position, enterFromEnd),
  );
  const [previousPosition, setPreviousPosition] = useState(position);
  if (previousPosition !== position) {
    setPreviousPosition(position);
    if (position) setMove(openingMove(file, position, false));
  }
  const [failure, setFailure] = useState<{ readonly move: Move; readonly message: string }>();
  const adjustment = useRef<
    { readonly range: Range; readonly top: number } | "start" | "end" | undefined
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
    else scroll.scrollTop += pending.range.getBoundingClientRect().top - pending.top;
  }, [pages]);

  const hasPages = Boolean(head);
  const approach = useEffectEvent((backwards: boolean) => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    if (!element || !scroll || !head || !tail || move || failure) return;
    const box = element.getBoundingClientRect();
    const view = scroll.getBoundingClientRect();
    if (box.bottom < view.top || box.top > view.bottom) return;
    const ahead = scroll.clientHeight * 2;
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

  useEffect(() => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    if (!element || !scroll) return;
    let previous = scroll.scrollTop;
    let backwards: boolean | undefined;
    const onScroll = () => {
      const next = scroll.scrollTop;
      if (next !== previous) {
        backwards = next < previous;
        approach(backwards);
      }
      previous = next;
    };
    const onSelection = () => {
      if (backwards !== undefined && getSelection()?.isCollapsed) approach(backwards);
    };
    scroll.addEventListener("scroll", onScroll, { passive: true });
    document.addEventListener("selectionchange", onSelection);
    return () => {
      scroll.removeEventListener("scroll", onScroll);
      document.removeEventListener("selectionchange", onSelection);
    };
  }, [hasPages]);

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
      {failure?.move.direction !== "after" && notice}
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
          {head.total === 0 ? (
            <Empty title="This file has an empty patch" />
          ) : (
            <pre
              className="patch-source"
              tabIndex={0}
              ref={pre}
              data-patch-etag={head.etag}
              aria-label="Patch source"
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
      {failure?.move.direction === "after" && notice}
    </>
  );
}

function openingMove(file: PatchFile, position: Props["position"], enterFromEnd: boolean): Move {
  if (position && position !== "start" && (!file.patchETag || position.etag === file.patchETag))
    return { range: { offset: position.first, etag: position.etag }, byte: position.byte };
  return { range: !position && enterFromEnd ? { last: true } : { offset: 0 } };
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
