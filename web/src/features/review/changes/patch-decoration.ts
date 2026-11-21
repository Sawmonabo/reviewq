import type { PatchRow } from "./patch-rows";

const ROW_HEIGHT = 21;
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

export type PatchDecoration = {
  readonly element: HTMLElement;
  readonly range: Range;
  readonly syntax: readonly Range[];
  readonly kind: PatchRow["kind"];
};

export function decoratePatchRow(row: PatchRow, range: Range): PatchDecoration {
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
  highlight(`reviewq-patch-${row.kind}`)?.add(range);
  const syntax: Range[] = [];
  if (row.old !== undefined || row.new !== undefined) {
    const text = range.toString();
    for (const token of text.matchAll(
      /\/\/.*|"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\b(?:as|async|await|const|else|export|fn|for|if|import|let|match|private|pub|readonly|return|type|use)\b/g,
    )) {
      const piece = new Range();
      piece.setStart(range.startContainer, range.startOffset + token.index);
      piece.setEnd(range.startContainer, range.startOffset + token.index + token[0].length);
      syntax.push(piece);
      highlight("reviewq-patch-syntax")?.add(piece);
    }
  }
  return { element, range, syntax, kind: row.kind };
}

export function positionPatchRow(element: HTMLElement, top: number, height: number) {
  element.style.top = `${top}px`;
  element.style.height = `${Math.max(ROW_HEIGHT, height)}px`;
}

export function releasePatchDecoration(decoration: PatchDecoration) {
  paint.get(`reviewq-patch-${decoration.kind}`)?.delete(decoration.range);
  for (const token of decoration.syntax) paint.get("reviewq-patch-syntax")?.delete(token);
  decoration.element.remove();
}

function highlight(name: string) {
  if (typeof CSS.highlights === "undefined" || typeof Highlight === "undefined") return;
  let current = paint.get(name);
  if (!current) {
    current = new Highlight();
    if (name === "reviewq-patch-syntax") current.priority = 1;
    paint.set(name, current);
    CSS.highlights.set(name, current);
  }
  return current;
}
