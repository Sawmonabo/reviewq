import type { PatchRow } from "./patch-rows";
import { patchPaintRange } from "./patch/text";
import type { PatchText } from "./patch/text";

const ROW_HEIGHT = 21;
const SYNTAX =
  /\/\/.*|"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\b(?:as|async|await|break|class|const|else|export|false|fn|for|from|function|if|import|let|match|new|private|pub|readonly|return|true|type|undefined|use|void)\b/g;
const paint = new Map<string, Highlight>();

export function setPatchMatch(ranges: readonly Range[]) {
  if (typeof CSS.highlights === "undefined" || typeof Highlight === "undefined") return;
  if (ranges.length) {
    const highlight = new Highlight(...ranges);
    highlight.priority = 2;
    CSS.highlights.set("reviewq-patch-match", highlight);
  } else CSS.highlights.delete("reviewq-patch-match");
}

export function keepSplitMatch(text: Text, offset: number) {
  const active = CSS.highlights?.get("reviewq-patch-match");
  if (!active) return undefined;
  const positions = [...active].map((range) => ({
    start: range.startContainer,
    from: range.startOffset,
    end: range.endContainer,
    to: range.endOffset,
  }));
  return (tail: Text) => {
    const ranges = positions.map(({ start, from, end, to }) => {
      const range = new Range();
      const moveStart = start === text && from >= offset;
      const moveEnd = end === text && to >= offset;
      range.setStart(moveStart ? tail : start, moveStart ? from - offset : from);
      range.setEnd(moveEnd ? tail : end, moveEnd ? to - offset : to);
      return range;
    });
    setPatchMatch(ranges);
  };
}

type PaintedRange = { readonly highlight: Highlight; readonly range: AbstractRange };

export type PatchDecoration = {
  readonly element: HTMLElement;
  readonly colors: readonly PaintedRange[];
};

export function decoratePatchRow(
  row: PatchRow,
  nodes: PatchText,
  from: number,
  to: number,
  text: string,
): PatchDecoration {
  const element = document.createElement("span");
  element.className = `patch-row patch-${row.kind}`;
  if (row.old !== undefined) element.dataset.old = String(row.old);
  if (row.new !== undefined) element.dataset.new = String(row.new);
  if (row.old !== undefined || row.new !== undefined) {
    element.setAttribute(
      "aria-label",
      [
        row.old !== undefined && `Old line ${row.old}`,
        row.new !== undefined && `New line ${row.new}`,
      ]
        .filter(Boolean)
        .join(", "),
    );
  }
  const colors: PaintedRange[] = [];
  if (row.kind !== "context")
    addColor(colors, highlight(`reviewq-patch-${row.kind}`), nodes, from, to);
  if ((row.kind === "add" || row.kind === "delete") && row.byte === row.rowStart) {
    // Paint the source sign in the center of its gutter without moving the text or selection.
    addColor(colors, highlight(`reviewq-patch-${row.kind}-sign`, 1), nodes, from, from + 1);
  }
  if (row.old !== undefined || row.new !== undefined) {
    for (const token of text.matchAll(SYNTAX)) {
      const first = token[0][0];
      const kind =
        first === "/" ? "comment" : first === "'" || first === '"' ? "string" : "keyword";
      const start = from + token.index;
      addColor(
        colors,
        highlight(`reviewq-patch-syntax-${kind}`, 1),
        nodes,
        start,
        start + token[0].length,
      );
    }
  }
  return { element, colors };
}

export function positionPatchRow(element: HTMLElement, top: number, height: number) {
  element.style.top = `${top}px`;
  element.style.height = `${Math.max(ROW_HEIGHT, height)}px`;
}

export function releasePatchDecoration(decoration: PatchDecoration) {
  for (const { highlight, range } of decoration.colors) highlight.delete(range);
  decoration.element.remove();
}

export function releasePatchMatch(host: HTMLElement) {
  const active = CSS.highlights?.get("reviewq-patch-match");
  if (!active) return;
  for (const range of active) if (host.contains(range.startContainer)) active.delete(range);
}

function addColor(
  colors: PaintedRange[],
  highlight: Highlight | undefined,
  nodes: PatchText,
  from: number,
  to: number,
) {
  if (!highlight) return;
  const range = patchPaintRange(nodes, from, to);
  highlight.add(range);
  colors.push({ highlight, range });
}

function highlight(name: string, priority = 0) {
  if (typeof CSS.highlights === "undefined" || typeof Highlight === "undefined") return;
  let current = paint.get(name);
  if (!current) {
    current = new Highlight();
    current.priority = priority;
    paint.set(name, current);
    CSS.highlights.set(name, current);
  }
  return current;
}
