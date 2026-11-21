import type { PatchFile } from "../../../api/generated/contracts";
import {
  decoratePatchRow,
  keepSplitMatch,
  positionPatchRow,
  releasePatchDecoration,
  releasePatchMatch,
} from "./patch-decoration";
import type { PatchDecoration } from "./patch-decoration";
import { indexAtByte, patchRows, utf8Length } from "./patch-rows";
import type { PatchRow } from "./patch-rows";

const ROWS_PER_BLOCK = 96;
const EXTRA_HUNK_BLOCKS = 32;
const AHEAD_PX = 240;
const BEHIND_PX = 100;

type BlockLayout = {
  readonly width: number;
  readonly height: number;
  readonly lineHeight: number;
  readonly textInset: number;
  readonly positions: number | Float64Array;
};
type TextStyle = { readonly lineHeight: number; readonly inlinePadding: number };
type Segment = {
  readonly element: HTMLDivElement;
  readonly text: Text;
  readonly first: number;
  end: number;
  layout: BlockLayout | undefined;
  positioned: BlockLayout | undefined;
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
  readonly readingSource: (
    top: number,
  ) => { readonly byte: number; readonly inset: number } | undefined;
  readonly sourceRange: (byte: number) => Range | undefined;
  readonly sourceByte: (node: Node, offset: number) => number | undefined;
  readonly hasAnchor: (side: Line["side"], number: number) => boolean;
  readonly anchor: (side: Line["side"], number: number) => HTMLElement | undefined;
  readonly copy: (range: Range) => string | undefined;
  readonly release: () => void;
};

const documents = new WeakMap<HTMLElement, PatchDocument>();

export function patchDocument(host: HTMLElement) {
  return documents.get(host);
}

export function mountPatchDocument(
  host: HTMLDivElement,
  source: string,
  file: Pick<PatchFile, "oldPath" | "newPath" | "created" | "deleted">,
  fragments?: readonly PatchRow[],
): PatchDocument {
  const rows = fragments ?? patchRows(source);
  const headers =
    (!fragments || fragments[0]?.byte === 0) && "onbeforematch" in HTMLElement.prototype
      ? duplicateHeaders(rows, source, file)
      : -1;
  // A patch with many tiny hunks still needs compact text blocks for native Find.
  let hunkBudget = Math.ceil(rows.length / ROWS_PER_BLOCK) + EXTRA_HUNK_BLOCKS;
  for (const row of rows) {
    if (row.kind !== "hunk") continue;
    hunkBudget -= 1;
    if (hunkBudget < 0) break;
  }
  const separateHunks = hunkBudget >= 0;
  const segments: Segment[] = [];
  const byElement = new Map<HTMLElement, Segment>();
  const oldLines = new Map<number, number>();
  const newLines = new Map<number, number>();
  const anchors = new Map<number, HTMLElement>();
  const scroller = host.closest<HTMLElement>(".drawer-body");
  const controls = host.closest(".changes")?.querySelector(".changes-header");
  const root = document.createDocumentFragment();
  const watched = new Set<Segment>();
  const geometryRange = new Range();
  let backwards = false;
  let previousOffset = scroller?.scrollTop ?? 0;
  let frame = 0;

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const segment = byElement.get(entry.target as HTMLElement);
        if (!segment) continue;
        if (entry.isIntersecting) watched.add(segment);
        else {
          watched.delete(segment);
          removeDecorations(segment);
          segment.layout = undefined;
          segment.positioned = undefined;
        }
      }
      requestDecoration();
    },
    { root: scroller, rootMargin: `${AHEAD_PX}px 0px` },
  );

  const resize = new ResizeObserver(requestDecoration);

  for (let start = 0; start < rows.length;) {
    const fileHeaders = start === headers;
    const hunk = separateHunks && rows[start]!.kind === "hunk";
    const continuation = rows[start]!.byte !== rows[start]!.rowStart;
    let end = start + (fileHeaders ? 2 : 1);
    if (!fileHeaders && !hunk && !continuation) {
      const limit = Math.min(start + ROWS_PER_BLOCK, rows.length);
      while (end < limit && end !== headers && !(separateHunks && rows[end]!.kind === "hunk"))
        end++;
    }
    const segment = createSegment(
      start,
      end,
      document.createTextNode(source.slice(rows[start]!.start, rows[end - 1]!.end)),
    );
    if (fileHeaders) {
      segment.element.classList.add("patch-file-headers");
      segment.element.setAttribute("hidden", "until-found");
    }
    if (hunk) segment.element.classList.add("patch-hunk-block");
    if (continuation) segment.element.classList.add("patch-continuation-block");
    segments.push(segment);
    root.append(segment.element);
    start = end;
  }
  rows.forEach((row, ordinal) => {
    if (row.old !== undefined) oldLines.set(row.old, ordinal);
    if (row.new !== undefined) newLines.set(row.new, ordinal);
  });
  host.replaceChildren(root);
  for (const segment of segments) observer.observe(segment.element);
  resize.observe(host);
  scroller?.addEventListener("scroll", onScroll, { passive: true });
  window.addEventListener("resize", requestDecoration);
  host.dataset.patchPrepared = "";

  const handle: PatchDocument = {
    line(line) {
      const ordinal = rowNumber(line.side, line.number);
      if (ordinal === undefined) return;
      const segment = segmentAt(ordinal);
      if (!segment) return;
      const rect = rowRange(segment, ordinal, line.column).getBoundingClientRect();
      let decoration = segment.decorations.get(ordinal);
      const bounds = segment.element.getBoundingClientRect();
      const layout = blockLayout(segment, bounds, textStyle(segment));
      if (!decoration || segment.positioned !== layout) {
        const index = ordinal - segment.first;
        const top = blockRowTop(segment, index, bounds, layout);
        const bottom = blockRowTop(segment, index + 1, bounds, layout);
        decoration ??= addDecoration(segment, ordinal, segment.element);
        positionPatchRow(decoration.element, top, bottom - top);
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
        match.start <= row.rowStart ||
        match.end > row.rowEnd ||
        match.end <= match.start
      )
        return;
      const original = source.slice(row.start, row.end).replace(/\r?\n$/, "");
      const from = indexAtByte(original, Math.max(0, match.start - row.byte));
      const to = indexAtByte(original, Math.min(row.byteEnd, match.end) - row.byte);
      const segment = segmentAt(ordinal);
      if (from === undefined || to === undefined || from >= to || !segment) return;
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
    readingSource(top) {
      const point = caret(top + 1);
      if (!point) return;
      const row = rows[point.ordinal]!;
      return {
        byte: row.byte + utf8Length(source.slice(row.start, row.start + point.column)),
        inset: point.range.getBoundingClientRect().top - top,
      };
    },
    sourceRange(byte) {
      if (!rows.length || byte < rows[0]!.byte || byte > rows.at(-1)!.byteEnd) return;
      const ordinal = rowAt(byte, "byte");
      const row = rows[ordinal]!;
      const column = indexAtByte(source.slice(row.start, row.end), byte - row.byte);
      const segment = segmentAt(ordinal);
      if (column === undefined || !segment) return;
      const range = new Range();
      range.setStart(segment.text, row.start - rows[segment.first]!.start + column);
      range.collapse(true);
      return range;
    },
    sourceByte(node, offset) {
      const location = textOffset(node, offset);
      if (location === undefined) return;
      const row = rows[rowAt(location, "start")]!;
      return row.byte + utf8Length(source.slice(row.start, location));
    },
    hasAnchor(side, number) {
      const ordinal = rowNumber(side, number);
      return ordinal !== undefined && rows[ordinal]!.byteEnd === rows[ordinal]!.rowEnd;
    },
    anchor(side, number) {
      const ordinal = rowNumber(side, number);
      if (ordinal === undefined || rows[ordinal]!.byteEnd !== rows[ordinal]!.rowEnd) return;
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
      scroller?.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", requestDecoration);
      releasePatchMatch(host);
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
    const segment: Segment = {
      element,
      text,
      first,
      end,
      layout: undefined,
      positioned: undefined,
      decorations: new Map(),
    };
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
    const restoreMatch = keepSplitMatch(segment.text, split);
    removeDecorations(segment, true);
    const tail = segment.text.splitText(split);
    const next = createSegment(first, segment.end, tail);
    segment.end = first;
    const location = segments.indexOf(segment);
    segments.splice(location + 1, 0, next);
    segment.element.after(next.element);
    observer.observe(next.element);
    segment.layout = undefined;
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
    restoreMatch?.(tail);
    requestDecoration();
  }

  function rowNumber(side: Line["side"], line: number) {
    return (side === "old" ? oldLines : newLines).get(line);
  }

  function rowAt(position: number, key: "byte" | "start") {
    let low = 0;
    let high = rows.length;
    while (low + 1 < high) {
      const middle = (low + high) >>> 1;
      if (rows[middle]![key] <= position) low = middle;
      else high = middle;
    }
    return low;
  }

  function segmentAt(ordinal: number) {
    return segments.find((segment) => segment.first <= ordinal && ordinal < segment.end);
  }

  function rowRange(segment: Segment, ordinal: number, column?: number) {
    const row = rows[ordinal]!;
    const offset = rows[segment.first]!.start;
    const start = row.start - offset;
    const end = contentEnd(row.end) - offset;
    if (column === undefined) {
      geometryRange.setStart(segment.text, start);
      geometryRange.setEnd(segment.text, end);
    } else {
      geometryRange.setStart(segment.text, Math.min(start + column, end));
      geometryRange.collapse(true);
    }
    return geometryRange;
  }

  function contentEnd(end: number) {
    if (source[end - 1] !== "\n") return end;
    return source[end - 2] === "\r" ? end - 2 : end - 1;
  }

  function textStyle(segment: Segment): TextStyle {
    const style = getComputedStyle(segment.text.parentElement!);
    return {
      lineHeight: Number.parseFloat(style.lineHeight),
      inlinePadding: Number.parseFloat(style.paddingLeft) + Number.parseFloat(style.paddingRight),
    };
  }

  function blockLayout(
    segment: Segment,
    bounds: DOMRect,
    { inlinePadding, lineHeight }: TextStyle,
  ) {
    const width = bounds.width - inlinePadding;
    const previous = segment.layout;
    if (
      previous &&
      previous.width === width &&
      previous.height === bounds.height &&
      Object.is(previous.lineHeight, lineHeight)
    )
      return previous;
    const length = segment.end - segment.first;
    const uniform = lineHeight > 0 && Math.abs(bounds.height - length * lineHeight) < 0.5;
    const layout = {
      width,
      height: bounds.height,
      lineHeight,
      textInset: uniform
        ? 0
        : rowRange(segment, segment.first, 0).getBoundingClientRect().top - bounds.top,
      positions: uniform ? lineHeight : new Float64Array(length).fill(Number.NaN),
    };
    segment.layout = layout;
    return layout;
  }

  function blockRowTop(segment: Segment, index: number, bounds: DOMRect, layout: BlockLayout) {
    if (index === 0) return 0;
    if (index === segment.end - segment.first) return bounds.height;
    const { positions } = layout;
    if (typeof positions === "number") return index * positions;
    const cached = positions[index]!;
    if (!Number.isNaN(cached)) return cached;
    // Wrapped text still uses its native geometry. Seek the viewport by row
    // and measure only its boundary probes and visible/nearby rows.
    const top = rowRange(segment, segment.first + index, 0).getBoundingClientRect().top;
    const relative = Math.max(0, top - bounds.top - layout.textInset);
    positions[index] = relative;
    return relative;
  }

  function rowAtOffset(
    segment: Segment,
    length: number,
    bounds: DOMRect,
    offset: number,
    layout: BlockLayout,
  ) {
    let low = 0;
    let high = length;
    while (low < high) {
      const mid = (low + high) >>> 1;
      if (blockRowTop(segment, mid + 1, bounds, layout) <= offset) low = mid + 1;
      else high = mid;
    }
    return low;
  }

  function addDecoration(
    segment: Segment,
    ordinal: number,
    parent: HTMLElement | DocumentFragment,
  ) {
    const row = rows[ordinal]!;
    const base = rows[segment.first]!.start;
    const end = contentEnd(row.end);
    const decoration = decoratePatchRow(
      row,
      segment.text,
      row.start - base,
      end - base,
      source.slice(row.start, end),
    );
    if (ordinal + 1 === segment.end) decoration.element.dataset.segmentEnd = "";
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
    if (frame || !watched.size) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      const view = scroller?.getBoundingClientRect();
      const top =
        Math.max(view?.top ?? 0, controls?.getBoundingClientRect().bottom ?? 0) -
        (backwards ? AHEAD_PX : BEHIND_PX);
      const bottom = (view?.bottom ?? window.innerHeight) + (backwards ? BEHIND_PX : AHEAD_PX);

      // Read each block's layout before adding children or styling any markers.
      const reading = [...watched];
      const firstSegment = reading[0];
      if (!firstSegment) return;
      const style = textStyle(firstSegment);
      const measurements = reading.map((segment) => {
        const bounds = segment.element.getBoundingClientRect();
        const length = segment.end - segment.first;
        if (bounds.bottom <= top || bounds.top >= bottom)
          return { segment, layout: undefined, positions: [], from: segment.end, to: segment.end };
        const layout = blockLayout(segment, bounds, style);
        const first = rowAtOffset(segment, length, bounds, top - bounds.top, layout);
        const last = Math.min(
          length,
          rowAtOffset(segment, length, bounds, bottom - bounds.top, layout) + 1,
        );
        const positions: number[] = [];
        for (let index = first; index <= last; index++)
          positions.push(blockRowTop(segment, index, bounds, layout));
        return {
          segment,
          layout,
          positions,
          from: segment.first + first,
          to: segment.first + last,
        };
      });
      for (const { segment, layout, positions, from, to } of measurements) {
        const changed = segment.positioned !== layout;
        for (const [ordinal, decoration] of segment.decorations) {
          if ((ordinal >= from && ordinal < to) || decoration.element === document.activeElement)
            continue;
          releasePatchDecoration(decoration);
          segment.decorations.delete(ordinal);
        }
        let additions: DocumentFragment | undefined;
        for (let ordinal = from; ordinal < to; ordinal++) {
          const index = ordinal - from;
          const existing = segment.decorations.get(ordinal);
          if (existing && !changed) continue;
          const decoration =
            existing ??
            addDecoration(segment, ordinal, (additions ??= document.createDocumentFragment()));
          const start = positions[index]!;
          positionPatchRow(decoration.element, start, positions[index + 1]! - start);
        }
        if (additions) segment.element.append(additions);
        segment.positioned = layout;
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
    const ordinal = rowAt(location, "start");
    const range = new Range();
    range.setStart(position.offsetNode, position.offset);
    range.collapse(true);
    return { ordinal, column: location - rows[ordinal]!.start, range };
  }

  function onScroll() {
    if (scroller) {
      const current = scroller.scrollTop;
      if (current !== previousOffset) backwards = current < previousOffset;
      previousOffset = current;
    }
    requestDecoration();
  }
}

function duplicateHeaders(
  rows: readonly PatchRow[],
  source: string,
  file: Pick<PatchFile, "oldPath" | "newPath" | "created" | "deleted">,
): number {
  const firstHunk = rows.findIndex((row) => row.kind === "hunk");
  if (firstHunk < 2) return -1;
  const oldRow = rows[firstHunk - 2]!;
  const newRow = rows[firstHunk - 1]!;
  const oldHeader = source.slice(oldRow.start, oldRow.end).replace(/\r?\n$/, "");
  const newHeader = source.slice(newRow.start, newRow.end).replace(/\r?\n$/, "");
  const oldPath = file.created ? "/dev/null" : `a/${file.oldPath}`;
  const newPath = file.deleted ? "/dev/null" : `b/${file.newPath}`;
  return oldHeader === `--- ${oldPath}` && newHeader === `+++ ${newPath}` ? firstHunk - 2 : -1;
}
