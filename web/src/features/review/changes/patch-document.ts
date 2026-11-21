import {
  decoratePatchRow,
  keepSplitMatch,
  positionPatchRow,
  releasePatchDecoration,
} from "./patch-decoration";
import type { PatchDecoration } from "./patch-decoration";
import { indexAtByte, patchRows } from "./patch-rows";

const ROWS_PER_BLOCK = 96;
const NEARBY_PX = 280;

type Segment = {
  readonly element: HTMLDivElement;
  readonly text: Text;
  readonly first: number;
  end: number;
  readonly decorations: Map<number, PatchDecoration>;
};
type Line = {
  readonly side: "old" | "new";
  readonly number: number;
  readonly column?: number | undefined;
};

type Match = {
  readonly line: Line;
  readonly hunk: number;
  readonly start: number;
  readonly end: number;
};

export type PatchDocument = {
  readonly line: (
    line: Line,
  ) => { readonly element: HTMLElement; readonly rect: DOMRect } | undefined;
  readonly match: (match: Match) => Range | undefined;
  readonly readingLine: (top: number) => (Line & { readonly inset: number }) | undefined;
  readonly anchor: (side: Line["side"], number: number) => HTMLElement | undefined;
  readonly copy: (range: Range) => string | undefined;
  readonly release: () => void;
};

const documents = new WeakMap<HTMLElement, PatchDocument>();

export function patchDocument(host: HTMLElement) {
  return documents.get(host);
}

export function mountPatchDocument(host: HTMLDivElement, source: string): PatchDocument {
  const rows = patchRows(source);
  const segments: Segment[] = [];
  const byElement = new Map<HTMLElement, Segment>();
  const oldLines = new Map<number, number>();
  const newLines = new Map<number, number>();
  const anchors = new Map<number, HTMLElement>();
  const scroller = host.closest<HTMLElement>(".drawer-body");
  const controls = host.closest(".changes")?.querySelector(".changes-header");
  const root = document.createDocumentFragment();
  const watched = new Set<Segment>();
  const dirty = new Set<Segment>();
  let activeRange:
    { readonly range: Range; readonly inset: number; readonly offset: number } | undefined;
  let width = host.clientWidth;
  let frame = 0;

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const segment = byElement.get(entry.target as HTMLElement);
        if (!segment) continue;
        if (entry.isIntersecting) {
          watched.add(segment);
          dirty.add(segment);
        } else {
          watched.delete(segment);
          dirty.delete(segment);
          removeDecorations(segment);
        }
      }
      requestDecoration();
    },
    { root: scroller, rootMargin: `${NEARBY_PX}px 0px` },
  );

  const resize = new ResizeObserver(() => {
    const anchor = activeRange;
    if (
      anchor &&
      scroller &&
      Math.abs(scroller.scrollTop - anchor.offset) < 1 &&
      anchor.range.startContainer.isConnected
    ) {
      const box = host.getBoundingClientRect();
      const delta = anchor.range.getBoundingClientRect().top - box.top - anchor.inset;
      if (Math.abs(delta) > 0.5) scroller.scrollTop += delta;
    }
    if (width !== host.clientWidth) {
      width = host.clientWidth;
      for (const segment of watched) dirty.add(segment);
      requestDecoration();
    }
    rememberReading();
  });

  for (let start = 0; start < rows.length; start += ROWS_PER_BLOCK) {
    const end = Math.min(start + ROWS_PER_BLOCK, rows.length);
    const segment = createSegment(
      start,
      end,
      document.createTextNode(source.slice(rows[start]!.start, rows[end - 1]!.end)),
    );
    segments.push(segment);
    root.append(segment.element);
  }
  rows.forEach((row, ordinal) => {
    if (row.old !== undefined) oldLines.set(row.old, ordinal);
    if (row.new !== undefined) newLines.set(row.new, ordinal);
  });
  host.replaceChildren(root);
  for (const segment of segments) observer.observe(segment.element);
  resize.observe(host);
  scroller?.addEventListener("scroll", rememberReading, { passive: true });
  rememberReading();
  host.dataset.patchPrepared = "";

  const handle: PatchDocument = {
    line(line) {
      const ordinal = rowNumber(line.side, line.number);
      if (ordinal === undefined) return;
      const segment = segmentAt(ordinal);
      if (!segment) return;
      const rect = rowRange(segment, ordinal, line.column).getBoundingClientRect();
      let decoration = segment.decorations.get(ordinal);
      if (!decoration) {
        const bounds = segment.element.getBoundingClientRect();
        const top = rowTop(segment, ordinal);
        const bottom = ordinal + 1 < segment.end ? rowTop(segment, ordinal + 1) : bounds.bottom;
        decoration = addDecoration(segment, ordinal, segment.element);
        positionPatchRow(decoration.element, Math.max(0, top - bounds.top - 3), bottom - top);
      }
      return { element: decoration.element, rect };
    },
    match(match) {
      const ordinal = rowNumber(match.line.side, match.line.number);
      const row = ordinal === undefined ? undefined : rows[ordinal];
      if (
        !row ||
        ordinal === undefined ||
        row.hunk !== match.hunk ||
        match.start <= row.byte ||
        match.end <= match.start
      )
        return;
      const original = source.slice(row.start, row.end).replace(/\r?\n$/, "");
      const from = indexAtByte(original, match.start - row.byte);
      const to = indexAtByte(original, match.end - row.byte);
      const segment = segmentAt(ordinal);
      if (from === undefined || to === undefined || !segment) return;
      const offset = row.start - rows[segment.first]!.start;
      const range = new Range();
      range.setStart(segment.text, offset + from);
      range.setEnd(segment.text, offset + to);
      return range;
    },
    readingLine(top) {
      const point = caret(top + 1);
      if (!point) return;
      const row = rows[point.ordinal]!;
      const side = row.new === undefined ? "old" : "new";
      const number = row[side];
      if (number === undefined) return;
      return {
        side,
        number,
        column: point.column,
        inset: point.range.getBoundingClientRect().top - top,
      };
    },
    anchor(side, number) {
      const ordinal = rowNumber(side, number);
      if (ordinal === undefined) return;
      const known = anchors.get(ordinal);
      if (known) return known;
      const segment = segmentAt(ordinal);
      if (!segment) return;
      if (ordinal + 1 < segment.end) splitAfter(segment, ordinal);
      const target = document.createElement("div");
      target.className = "patch-thread-slot";
      segment.element.after(target);
      anchors.set(ordinal, target);
      return target;
    },
    copy(range) {
      const start = textOffset(range.startContainer, range.startOffset);
      const end = textOffset(range.endContainer, range.endOffset);
      return start === undefined || end === undefined ? undefined : source.slice(start, end);
    },
    release() {
      cancelAnimationFrame(frame);
      observer.disconnect();
      resize.disconnect();
      scroller?.removeEventListener("scroll", rememberReading);
      for (const segment of segments) removeDecorations(segment, true);
      if (documents.get(host) === handle) {
        documents.delete(host);
        host.replaceChildren();
        delete host.dataset.patchPrepared;
      }
    },
  };
  documents.set(host, handle);
  return handle;

  function createSegment(first: number, end: number, text: Text): Segment {
    const element = document.createElement("div");
    element.className = "patch-block";
    const body = document.createElement("div");
    body.className = "patch-block-text";
    body.append(text);
    element.append(body);
    const segment = { element, text, first, end, decorations: new Map<number, PatchDecoration>() };
    byElement.set(element, segment);
    return segment;
  }

  function splitAfter(segment: Segment, ordinal: number) {
    const selection = getSelection();
    const previousSelection =
      selection && selection.anchorNode && selection.focusNode
        ? {
            anchor: selection.anchorNode,
            anchorOffset: selection.anchorOffset,
            focus: selection.focusNode,
            focusOffset: selection.focusOffset,
          }
        : undefined;
    const first = ordinal + 1;
    const split = rows[first]!.start - rows[segment.first]!.start;
    const reading = activeRange;
    const readingOffset =
      reading?.range.startContainer === segment.text ? reading.range.startOffset : undefined;
    const restoreMatch = keepSplitMatch(segment.text, split);
    removeDecorations(segment, true);
    const tail = segment.text.splitText(split);
    const next = createSegment(first, segment.end, tail);
    segment.end = first;
    const location = segments.indexOf(segment);
    segments.splice(location + 1, 0, next);
    segment.element.after(next.element);
    observer.observe(next.element);
    if (watched.has(segment)) dirty.add(segment);
    // Moving the split Text to its new wrapper retargets a DOM Selection to
    // the old parent. Reapply its original direction and source boundaries.
    if (
      selection &&
      previousSelection &&
      (previousSelection.anchor === segment.text || previousSelection.focus === segment.text)
    ) {
      const { anchor, anchorOffset, focus, focusOffset } = previousSelection;
      const moveAnchor = anchor === segment.text && anchorOffset > split;
      const moveFocus = focus === segment.text && focusOffset > split;
      selection.setBaseAndExtent(
        moveAnchor ? tail : anchor,
        moveAnchor ? anchorOffset - split : anchorOffset,
        moveFocus ? tail : focus,
        moveFocus ? focusOffset - split : focusOffset,
      );
    }
    if (reading && readingOffset !== undefined && readingOffset >= split) {
      const range = new Range();
      range.setStart(tail, readingOffset - split);
      range.collapse(true);
      activeRange = { ...reading, range };
    }
    restoreMatch?.(tail);
    requestDecoration();
  }

  function rowNumber(side: Line["side"], line: number) {
    return (side === "old" ? oldLines : newLines).get(line);
  }

  function segmentAt(ordinal: number) {
    return segments.find((segment) => segment.first <= ordinal && ordinal < segment.end);
  }

  function rowRange(segment: Segment, ordinal: number, column?: number) {
    const row = rows[ordinal]!;
    const offset = rows[segment.first]!.start;
    const start = row.start - offset;
    let end = row.end - offset;
    if (source[row.end - 1] === "\n") {
      end--;
      if (source[row.end - 2] === "\r") end--;
    }
    const range = new Range();
    if (column === undefined) {
      range.setStart(segment.text, start);
      range.setEnd(segment.text, end);
    } else {
      range.setStart(segment.text, Math.min(start + column, end));
      range.collapse(true);
    }
    return range;
  }

  function rowTop(segment: Segment, ordinal: number) {
    return rowRange(segment, ordinal, 0).getBoundingClientRect().top;
  }

  function addDecoration(
    segment: Segment,
    ordinal: number,
    parent: HTMLElement | DocumentFragment,
  ) {
    const decoration = decoratePatchRow(rows[ordinal]!, rowRange(segment, ordinal));
    segment.decorations.set(ordinal, decoration);
    parent.append(decoration.element);
    return decoration;
  }

  function removeDecorations(segment: Segment, force = false) {
    for (const [ordinal, decoration] of segment.decorations) {
      if (!force && decoration.element === document.activeElement) continue;
      releasePatchDecoration(decoration);
      segment.decorations.delete(ordinal);
    }
  }

  function requestDecoration() {
    if (frame || !dirty.size) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      // Read all positions before attaching decorations or writing styles.
      const measurements = [...dirty].map((segment) => {
        const bounds = segment.element.getBoundingClientRect();
        const tops: number[] = [];
        for (let ordinal = segment.first; ordinal < segment.end; ordinal++)
          tops.push(rowTop(segment, ordinal));
        tops.push(bounds.bottom);
        return { segment, top: bounds.top, tops };
      });
      dirty.clear();
      for (const { segment, top, tops } of measurements) {
        const additions = document.createDocumentFragment();
        for (let index = 0; index + 1 < tops.length; index++) {
          const ordinal = segment.first + index;
          const start = tops[index]!;
          const decoration =
            segment.decorations.get(ordinal) ?? addDecoration(segment, ordinal, additions);
          positionPatchRow(
            decoration.element,
            Math.max(0, start - top - 3),
            tops[index + 1]! - start,
          );
        }
        segment.element.append(additions);
      }
    });
  }

  function textOffset(node: Node, offset: number) {
    const container = node.parentElement?.closest<HTMLElement>(".patch-block");
    const segment = container && byElement.get(container);
    return segment && segment.text === node ? rows[segment.first]!.start + offset : undefined;
  }

  function caret(top: number) {
    const bounds = host.getBoundingClientRect();
    if (top < bounds.top || top >= bounds.bottom || !document.caretPositionFromPoint) return;
    const gutter = Number.parseFloat(getComputedStyle(host).getPropertyValue("--patch-gutter"));
    const position = document.caretPositionFromPoint(bounds.left + gutter + 2, top);
    if (!position || !(position.offsetNode instanceof Text)) return;
    const location = textOffset(position.offsetNode, position.offset);
    if (location === undefined) return;
    let low = 0;
    let high = rows.length;
    while (low + 1 < high) {
      const middle = (low + high) >>> 1;
      if (rows[middle]!.start <= location) low = middle;
      else high = middle;
    }
    const range = new Range();
    range.setStart(position.offsetNode, position.offset);
    range.collapse(true);
    return { ordinal: low, column: location - rows[low]!.start, range };
  }

  function rememberReading() {
    if (!scroller) return;
    const bounds = host.getBoundingClientRect();
    const view = scroller.getBoundingClientRect();
    const top = Math.max(view.top, controls?.getBoundingClientRect().bottom ?? view.top);
    const sample = caret(top + 2);
    activeRange = sample
      ? {
          range: sample.range,
          inset: sample.range.getBoundingClientRect().top - bounds.top,
          offset: scroller.scrollTop,
        }
      : undefined;
  }
}
