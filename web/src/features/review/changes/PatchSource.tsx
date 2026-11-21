import { useCallback, useEffect, useEffectEvent, useLayoutEffect, useRef, useState } from "react";
import { PatchSourceChanged } from "../../../api/changes";
import type { PatchRange } from "../../../api/changes";
import { ApiResponseError } from "../../../api/client";
import type { PatchDiscussion, PatchFile } from "../../../api/generated/contracts";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { PatchDocumentView } from "./PatchDocumentView";
import { copyPatchRange } from "./patch/copy-range";
import { createPatchLookahead, sourceTargetKey } from "./patch/lookahead";
import type { PatchReadQueue } from "./patch/read-queue";
import { readSourcePage } from "./patch/source-page";
import type { SourcePage } from "./patch/source-page";
import type { PatchScroll } from "./patch/usePatchScroll";
import { patchDocument } from "./patch-document";
import { sourceReadingTop } from "./patch-position";
import type { PatchLine, SourcePosition } from "./patch-position";
import { indexAtByte } from "./patch-rows";

type Move = {
  readonly range: PatchRange;
  readonly byte?: number | undefined;
  readonly end?: number | undefined;
  readonly line?: PatchLine | undefined;
  readonly predictedRow?: number | undefined;
  readonly explicit?: boolean;
  readonly direction?: "before" | "after";
  readonly entry?: "start" | "end";
  readonly land?: "start" | "end";
};
type Props = {
  readonly file: PatchFile;
  readonly read: PatchReadQueue;
  readonly scroll: PatchScroll;
  readonly threads: readonly PatchDiscussion[];
  readonly position: SourcePosition | "start" | "away" | undefined;
  readonly line: PatchLine | undefined;
  readonly entrySide: "start" | "end" | undefined;
  readonly onSourceChanged: () => void;
};

export function PatchSource({
  file,
  read: queued,
  scroll: patchScroll,
  threads,
  position,
  line,
  entrySide,
  onSourceChanged,
}: Props) {
  const pre = useRef<HTMLElement>(null);
  const attach = useCallback((element: HTMLElement | null) => {
    pre.current = element;
  }, []);
  const lastScroll = useRef(0);
  const reentry = useRef(entrySide);
  useLayoutEffect(() => {
    if (entrySide) reentry.current = entrySide;
    if (position && position !== "away") reentry.current = undefined;
  }, [entrySide, position]);
  const [pages, setPages] = useState<readonly SourcePage[]>([]);
  const [lookahead] = useState(createPatchLookahead);
  const [heldPages, setHeldPages] = useState<readonly SourcePage[]>([]);
  const [copyError, setCopyError] = useState(false);
  const copying = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => copying.current?.abort(), []);
  useEffect(() => () => lookahead.clear(), [lookahead]);
  const direction = useRef(entrySide === "end");
  const [move, setMove] = useState<Move | undefined>(() =>
    openingMove(file, position, line, entrySide === "end"),
  );
  const [failure, setFailure] = useState<{
    readonly move: Move;
    readonly message: string;
    readonly ready?: SourcePage[];
  }>();
  const [previousPosition, setPreviousPosition] = useState(position);
  const [previousLine, setPreviousLine] = useState(line);
  if (previousPosition !== position || previousLine !== line) {
    setPreviousPosition(position);
    setPreviousLine(line);
    if (move?.explicit && (!position || position === "away")) {
      setMove(
        pages.length ? undefined : openingMove(file, undefined, undefined, entrySide === "end"),
      );
      setFailure(undefined);
    } else if (position && position !== "away") {
      setFailure(undefined);
      const first = pages[0];
      const last = pages.at(-1);
      if (
        typeof position === "object" &&
        first &&
        last &&
        first.etag === position.etag &&
        first.first <= position.byte &&
        last.next >= (position.end ?? position.byte + 1)
      )
        setMove(undefined);
      else setMove(openingMove(file, position, line, false));
    }
  }
  const adjustment = useRef<"neighbor" | "start" | "end" | undefined>(undefined);
  const head = pages[0];
  const tail = pages.at(-1);
  const replacePages = (next: readonly SourcePage[]) => {
    const element = pre.current;
    patchScroll.preserve();
    const previous = new Map([...pages, ...heldPages].map((page) => [pageKey(page), page]));
    const loaded = next.map((page) => previous.get(pageKey(page)) ?? page);
    if (element && head?.rows) {
      const included = new Set(loaded.map(pageKey));
      const held = [...previous.values()].filter((page) => {
        const span = pageElement(element, page);
        return !included.has(pageKey(page)) && span && heldEndpoint(span);
      });
      setHeldPages(held);
    } else setHeldPages([]);
    setPages(loaded);
  };
  const finishPage = useEffectEvent((target: Move, loaded: SourcePage[]) => {
    const page = loaded[0]!;
    try {
      const element = pre.current;
      const scroll = element?.closest<HTMLElement>(".drawer-body");
      const backwards = target.direction === "before";
      if (target.direction && element && scroll && head && tail) {
        if (backwards ? page.next !== head.first : page.first !== tail.next)
          throw new Error("The adjacent patch range changed.");
        const dropped = pages.length === 3 ? (backwards ? tail : head) : undefined;
        const span = dropped && pageElement(element, dropped);
        if (span && (visiblePage(span, scroll) || (!head.rows && selected(span)))) {
          if (failure?.move === target && !failure.ready)
            setFailure({
              move: target,
              ready: loaded,
              message: selected(span)
                ? "Clear the selection to continue reading."
                : "Scroll past this text to continue reading.",
            });
          setMove(undefined);
          return;
        }
        reentry.current = undefined;
        replacePages(backwards ? [page, ...pages.slice(0, 2)] : [...pages.slice(-2), page]);
      } else {
        adjustment.current =
          target.entry === "end"
            ? "neighbor"
            : element?.closest(".selected-patch")?.contains(document.activeElement)
              ? target.land
              : undefined;
        if (target.entry === reentry.current) reentry.current = undefined;
        replacePages(loaded);
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
    const load = (range: PatchRange, line?: PatchLine) =>
      lookahead.measure(() =>
        queued(signal, "foreground", () => readSourcePage(file, signal, range, line)),
      );
    if (target.explicit || target.land || target.entry) lookahead.clear();
    let prepared = target.direction && (await lookahead.take(target.range, signal));
    if (!prepared && target.predictedRow !== undefined && file.patchETag) {
      const predicted = await lookahead.take(
        { row: target.predictedRow, etag: file.patchETag },
        signal,
      );
      if (predicted && "offset" in target.range && predicted.first === target.range.offset)
        prepared = predicted;
    }
    const result = [prepared || (await load(target.range, target.line))];
    let tail = result[0]!;
    const first = tail.rows?.[0];
    if (target.line && first && first.byteEnd < first.rowEnd)
      return [await load({ before: first.rowEnd, etag: tail.etag })];
    // Restore the same wrapping context for a bookmark inside a giant physical line.
    if (target.byte !== undefined && target.byte < tail.first)
      return [await load({ offset: target.byte, etag: tail.etag })];
    while (
      target.byte !== undefined &&
      tail.next < (target.end ?? target.byte + 1) &&
      tail.next < tail.total &&
      result.length < 3
    ) {
      tail = await load({ offset: tail.next, etag: tail.etag });
      result.push(tail);
    }
    if (
      target.byte !== undefined &&
      tail.next < (target.end ?? target.byte + 1) &&
      tail.next < tail.total
    )
      return [await load({ offset: target.byte, etag: tail.etag })];
    return result;
  });
  const sourceChanged = useEffectEvent(onSourceChanged);
  useLayoutEffect(() => {
    if (!move) return;
    const controller = new AbortController();
    void read(move, controller.signal).then(
      (page) => {
        if (!controller.signal.aborted) finishPage(move, page);
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        if (
          error instanceof PatchSourceChanged ||
          (error instanceof ApiResponseError && error.code === "precondition_failed")
        ) {
          setMove(undefined);
          sourceChanged();
          return;
        }
        setFailure({
          move,
          message: error instanceof Error ? error.message : "The patch range could not be loaded.",
        });
        setMove(undefined);
      },
    );
    return () => controller.abort();
  }, [move, file.reviewId, file.revision, file.fileId, file.patchETag]);

  useLayoutEffect(() => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    const pending = adjustment.current;
    adjustment.current = undefined;
    if (!element || !scroll) return;
    if (!pending) patchScroll.reconcile();
    if (pending === "end")
      patchScroll.move(
        scroll.scrollTop +
          element.getBoundingClientRect().bottom -
          scroll.getBoundingClientRect().bottom,
      );
    else if (pending === "start")
      patchScroll.move(
        scroll.scrollTop + element.getBoundingClientRect().top - sourceReadingTop(element, scroll),
      );
    else if (pending === "neighbor") {
      // The outer virtualizer moves the following file when this slot's new
      // measured height arrives. Give that measurement sole scroll ownership.
      const slot = element.closest<HTMLElement>(".file-stream-slot");
      if (slot) slot.dataset.sourceAnchorHeight = String(slot.offsetHeight);
    }
    lastScroll.current = scroll.scrollTop;
  }, [pages, patchScroll]);

  const resumeHeld = useEffectEvent(() => {
    if (failure?.ready) finishPage(failure.move, failure.ready);
  });
  const hasPages = Boolean(head);
  const approach = useEffectEvent((backwards: boolean, travel = 0, prepareOnly = false) => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    if (!element || !scroll || !head || !tail || failure) return;
    const box = element.getBoundingClientRect();
    const view = scroll.getBoundingClientRect();
    const ahead = lookahead.distance(scroll.clientHeight);
    const side = reentry.current;
    if (side && backwards === (side === "end")) {
      const close = backwards
        ? box.bottom >= view.top - ahead && box.bottom <= view.bottom + ahead
        : box.top <= view.bottom + ahead && box.top >= view.top - ahead;
      if (close && (backwards ? tail.next < tail.total : head.first > 0)) {
        if (prepareOnly || (!head.rows && selected(element))) return;
        if (move?.entry !== side)
          setMove({
            range: backwards ? { last: true, etag: head.etag } : { offset: 0, etag: head.etag },
            entry: side,
          });
        return;
      }
      if (close) reentry.current = undefined;
    }
    if ((side && backwards !== (side === "end")) || box.bottom < view.top || box.top > view.bottom)
      return;
    const before = { before: head.first, etag: head.etag };
    const after = { offset: tail.next, etag: tail.etag };
    const canBefore =
      head.first > 0 && view.top - box.top <= (backwards ? ahead : scroll.clientHeight);
    const canAfter =
      tail.next < tail.total &&
      box.bottom - view.bottom <= (backwards ? scroll.clientHeight : ahead);
    const primary = backwards ? before : after;
    const reserve = backwards ? after : before;
    const wantPrimary = backwards ? canBefore : canAfter;
    const wantReserve = backwards ? canAfter : canBefore;
    if (head.rows && !move?.explicit && !move?.land) {
      const targets = [
        ...(wantPrimary ? [primary] : []),
        ...(wantReserve ? [reserve] : []),
        ...(!backwards &&
        canAfter &&
        !tail.continuesLine &&
        tail.nextRow !== undefined &&
        tail.nextRow + 128 < (file.lineCount ?? 0)
          ? [{ row: tail.nextRow + 128, etag: tail.etag }]
          : []),
      ];
      const loadingKey = move ? sourceTargetKey(move.range) : undefined;
      lookahead.prepare(
        targets.filter((target) => sourceTargetKey(target) !== loadingKey),
        (target, signal) => queued(signal, "ahead", () => readSourcePage(file, signal, target)),
      );
    }
    const room = backwards ? view.top - box.top : box.bottom - view.bottom;
    if (prepareOnly || move || !wantPrimary || room > scroll.clientHeight + Math.abs(travel))
      return;
    const dropped = pages.length === 3 ? (backwards ? tail : head) : undefined;
    const span = dropped && pageElement(element, dropped);
    if (span && (visiblePage(span, scroll) || (!head.rows && selected(span)))) return;
    setMove({
      range: primary,
      direction: backwards ? "before" : "after",
      predictedRow: backwards || tail.continuesLine ? undefined : tail.nextRow,
    });
  });
  useEffect(() => {
    approach(direction.current, 0, true);
  }, [pages]);

  const keepSourceEdge = useEffectEvent(
    (element: HTMLElement, scroll: HTMLElement, previous: number, delta: number) => {
      const backwards = delta < 0;
      if (!head || !tail) return;
      const box = element.getBoundingClientRect();
      const top = sourceReadingTop(element, scroll);
      const bottom = scroll.getBoundingClientRect().bottom;
      const pendingEntry = move?.entry ?? failure?.move.entry;
      const entry = pendingEntry ?? reentry.current;
      if (
        entry &&
        (pendingEntry || backwards === (entry === "end")) &&
        (entry === "end" ? tail.next !== tail.total : head.first !== 0)
      ) {
        // A resize elsewhere can change scrollTop during the read. Keep the
        // actual source boundary outside the viewport until it has the right bytes.
        const correction = entry === "end" ? box.bottom - top : box.top - bottom;
        if (
          entry === "end" ? box.top < bottom && correction > 0 : box.bottom > top && correction < 0
        )
          patchScroll.move(scroll.scrollTop + correction);
        return;
      }
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
      const remaining = backwards ? top - (edge.top + delta) : edge.bottom + delta - bottom;
      if (Math.abs(delta) > Math.max(0, remaining))
        patchScroll.move(previous + Math.sign(delta) * Math.max(0, remaining));
    },
  );

  const clampWheel = useEffectEvent(
    (event: WheelEvent, element: HTMLElement, scroll: HTMLElement, delta: number) => {
      if (!head?.rows || !tail || !event.cancelable || event.defaultPrevented || event.ctrlKey)
        return;
      const backwards = delta < 0;
      if (backwards ? head.first === 0 : tail.next === tail.total) return;
      const box = element.getBoundingClientRect();
      const top = sourceReadingTop(element, scroll);
      const bottom = scroll.getBoundingClientRect().bottom;
      if (box.bottom <= top || box.top >= bottom) return;
      const remaining = Math.max(0, backwards ? top - box.top : box.bottom - bottom);
      if (Math.abs(delta) <= remaining) return;
      event.preventDefault();
      if (remaining > 0) patchScroll.move(scroll.scrollTop + Math.sign(delta) * remaining);
    },
  );

  useEffect(() => {
    const element = pre.current;
    const scroll = element?.closest<HTMLElement>(".drawer-body");
    if (!element || !scroll) return;
    lastScroll.current = scroll.scrollTop;
    let readerGesture = false;
    let backwards: boolean | undefined;
    let wheelDirection: boolean | undefined;
    const onScroll = () => {
      const delta = scroll.scrollTop - lastScroll.current;
      const reverse = delta < 0;
      if (delta) resumeHeld();
      if (
        delta &&
        readerGesture &&
        !patchScroll.moved(scroll.scrollTop) &&
        (wheelDirection === undefined || wheelDirection === reverse)
      ) {
        backwards = reverse;
        direction.current = backwards;
        lookahead.motion(delta);
        keepSourceEdge(element, scroll, lastScroll.current, delta);
        approach(backwards, delta);
      }
      lastScroll.current = scroll.scrollTop;
    };
    const onSelection = () => {
      resumeHeld();
      setHeldPages((current) => {
        const held = current.filter((page) => {
          const span = pageElement(element, page);
          return span && heldEndpoint(span);
        });
        return held.length === current.length ? current : held;
      });
      if (readerGesture && backwards !== undefined && getSelection()?.isCollapsed)
        approach(backwards);
    };
    const onWheel = (event: WheelEvent) => {
      if (event.ctrlKey) return;
      readerGesture = true;
      if (event.deltaY) {
        backwards = event.deltaY < 0;
        wheelDirection = backwards;
        direction.current = backwards;
        const delta =
          event.deltaY *
          (event.deltaMode === 1 ? 21 : event.deltaMode === 2 ? scroll.clientHeight : 1);
        lookahead.motion(delta);
        approach(backwards, delta);
        clampWheel(event, element, scroll, delta);
      }
    };
    const onTouch = () => {
      readerGesture = true;
      wheelDirection = undefined;
    };
    const onPointer = (event: PointerEvent) => {
      wheelDirection = undefined;
      readerGesture =
        event.target === scroll || (event.target instanceof Node && element.contains(event.target));
    };
    const onKey = (event: KeyboardEvent) => {
      wheelDirection = undefined;
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
    scroll.addEventListener(
      "scrollend",
      () => {
        readerGesture = false;
        wheelDirection = undefined;
      },
      options,
    );
    scroll.addEventListener("wheel", onWheel, { signal: signals.signal, passive: false });
    scroll.addEventListener("touchmove", onTouch, options);
    document.addEventListener("pointerdown", onPointer, options);
    document.addEventListener("keydown", onKey, options);
    document.addEventListener("selectionchange", onSelection, options);
    document.addEventListener("focusin", onSelection, options);
    return () => signals.abort();
  }, [hasPages, lookahead, patchScroll]);

  function seek(land: "start" | "end") {
    if (!head) return;
    setFailure(undefined);
    reentry.current = undefined;
    const range =
      land === "start" ? { offset: 0, etag: head.etag } : { last: true as const, etag: head.etag };
    setMove({ range, land });
  }

  const failedAtEnd = failure?.move.direction === "after" || failure?.move.entry === "end";
  const seeking = move && position && position !== "away" ? "pending" : undefined;
  const retained = [...heldPages, ...pages].sort((a, b) => a.first - b.first);
  const notice = failure && (
    <Notice {...(!move && !failure.ready && { retry: () => setMove(failure.move) })}>
      {move ? "Retrying the patch range…" : failure.message}
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
              <strong>{head.rows ? "Large patch" : "Large patch · source view"}</strong>
              <span className="meta">
                Bytes {head.first + 1}–{tail.next} of {tail.total}
              </span>
            </div>
            <div className="actions">
              <button type="button" onClick={() => seek("start")}>
                Start
              </button>
              <button type="button" onClick={() => seek("end")}>
                End
              </button>
              {tail.continuesLine && (
                <button
                  type="button"
                  onClick={() => {
                    setFailure(undefined);
                    reentry.current = undefined;
                    setMove({ range: { offset: tail.next, etag: tail.etag }, land: "start" });
                  }}
                >
                  Continue line
                </button>
              )}
            </div>
            <p className="meta">
              Browser Find searches the displayed range. Changes Find searches all available source.
            </p>
          </div>
          {!failedAtEnd && notice}
          {head.total === 0 ? (
            <Empty title="This file has an empty patch" />
          ) : head.rows ? (
            <div
              className="patch-source-window"
              tabIndex={0}
              ref={attach}
              data-source-seek={seeking}
              aria-label="Selectable unified patch"
              aria-busy={Boolean(move)}
              onCopy={(event) => {
                if (event.defaultPrevented) return;
                const selection = getSelection();
                if (!selection?.rangeCount || selection.isCollapsed) return;
                const range = selection.getRangeAt(0);
                if (!event.currentTarget.contains(range.commonAncestorContainer)) return;
                const from = sourceByte(range.startContainer, range.startOffset);
                const to = sourceByte(range.endContainer, range.endOffset);
                if (from === undefined || to === undefined) return;
                if (to <= from) return;
                event.preventDefault();
                copying.current?.abort();
                setCopyError(false);
                const text = copyLoadedPages(retained, from, to);
                if (text !== undefined) {
                  event.clipboardData.setData("text/plain", text);
                  return;
                }
                const controller = new AbortController();
                copying.current = controller;
                if (typeof ClipboardItem === "undefined" || !navigator.clipboard) {
                  setCopyError(true);
                  return;
                }
                const payload = copyPatchRange(file, from, to, controller.signal, queued);
                const failed = () => {
                  if (controller.signal.aborted) return;
                  controller.abort();
                  setCopyError(true);
                };
                void payload.catch(failed);
                try {
                  void navigator.clipboard
                    .write([new ClipboardItem({ "text/plain": payload })])
                    .catch(failed);
                } catch {
                  failed();
                }
              }}
            >
              {retained.map((page) => (
                <div
                  key={pageKey(page)}
                  data-source-start={page.first}
                  data-source-end={page.next}
                  className={heldPages.includes(page) ? "patch-window-held" : undefined}
                >
                  <PatchDocumentView
                    file={file}
                    text={page.text}
                    discussions={threads}
                    fragments={page.rows}
                  />
                </div>
              ))}
            </div>
          ) : (
            <pre
              className="patch-source"
              tabIndex={0}
              ref={attach}
              data-patch-etag={head.etag}
              data-source-seek={seeking}
              aria-label="Patch source"
              aria-busy={Boolean(move?.direction || move?.byte !== undefined)}
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
                <span
                  key={pageKey(page)}
                  data-source-start={page.first}
                  data-source-end={page.next}
                >
                  {page.text}
                </span>
              ))}
            </pre>
          )}
          {copyError && (
            <Notice>The selected source could not be copied. Try copying again.</Notice>
          )}
        </>
      )}
      {failedAtEnd && notice}
    </>
  );
}

function openingMove(
  file: PatchFile,
  position: Props["position"],
  line: PatchLine | undefined,
  entrySide: boolean,
): Move {
  if (typeof position === "object" && (!file.patchETag || position.etag === file.patchETag))
    return {
      range: { offset: position.first, etag: position.etag },
      byte: position.byte,
      end: position.end,
      explicit: true,
    };
  return {
    range: (!position || position === "away") && entrySide ? { last: true } : { offset: 0 },
    line,
    explicit: position === "start",
  };
}

function selected(element: Element) {
  const selection = getSelection();
  return Boolean(selection && !selection.isCollapsed && selection.containsNode(element, true));
}

function pageKey(page: SourcePage) {
  return `${page.first}:${page.next}`;
}

function pageElement(host: HTMLElement, page: SourcePage) {
  return host.querySelector(`[data-source-start="${page.first}"][data-source-end="${page.next}"]`);
}

function visiblePage(span: Element, scroll: HTMLElement) {
  const box = span.getBoundingClientRect();
  const view = scroll.getBoundingClientRect();
  return box.bottom > view.top && box.top < view.bottom;
}

function heldEndpoint(element: Element) {
  const selection = getSelection();
  return (
    element.contains(document.activeElement) ||
    Boolean(
      selection &&
      !selection.isCollapsed &&
      (element.contains(selection.anchorNode) || element.contains(selection.focusNode)),
    )
  );
}

function sourceByte(node: Node, offset: number) {
  const host = node.parentElement?.closest<HTMLElement>(".patch-rich");
  return host ? patchDocument(host)?.sourceByte(node, offset) : undefined;
}

function copyLoadedPages(pages: readonly SourcePage[], from: number, to: number) {
  const parts: string[] = [];
  let next = from;
  for (const page of pages) {
    if (page.next <= next) continue;
    if (page.first > next) break;
    const end = Math.min(page.next, to);
    const startIndex = indexAtByte(page.text, next - page.first);
    const endIndex = indexAtByte(page.text, end - page.first);
    if (startIndex === undefined || endIndex === undefined) return;
    parts.push(page.text.slice(startIndex, endIndex));
    if (end === to) return parts.join("");
    next = end;
  }
}
