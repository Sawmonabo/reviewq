type TextPart = { readonly node: Text; readonly start: number };

export type PatchText = Text | readonly TextPart[];

const TARGET_PART_UNITS = 4 * 1024;
const MIN_PART_UNITS = TARGET_PART_UNITS / 2;
let words: Intl.Segmenter | undefined;
let graphemes: Intl.Segmenter | undefined;

export function splitPatchText(source: string) {
  if (source.length <= TARGET_PART_UNITS || typeof Intl.Segmenter !== "function") return;
  words ??= new Intl.Segmenter(undefined, { granularity: "word" });
  graphemes ??= new Intl.Segmenter(undefined, { granularity: "grapheme" });
  const word = words.segment(source);
  const grapheme = graphemes.segment(source);
  let parts: TextPart[] | undefined;
  let start = 0;
  let target = TARGET_PART_UNITS;
  // Adjacent Text nodes keep native Find and selection in the same inline flow.
  while (target < source.length) {
    const piece = word.containing(target);
    if (!piece || source.length - piece.index < MIN_PART_UNITS) break;
    const boundary = piece.index;
    if (boundary - start < MIN_PART_UNITS || grapheme.containing(boundary)?.index !== boundary) {
      // Keep joining words and extended graphemes intact, even if a part is large.
      target = Math.max(target + TARGET_PART_UNITS, boundary + piece.segment.length);
      continue;
    }
    (parts ??= []).push({ node: document.createTextNode(source.slice(start, boundary)), start });
    start = boundary;
    target = start + TARGET_PART_UNITS;
  }
  if (!parts) return;
  parts.push({ node: document.createTextNode(source.slice(start)), start });
  return parts;
}

export function patchTextOffset(text: PatchText, node: Node, offset: number) {
  if (text instanceof Text) return node === text ? offset : undefined;
  const part = text.find((part) => part.node === node);
  return part && part.start + offset;
}

export function setPatchTextRange(range: Range, text: PatchText, from: number, to = from) {
  if (text instanceof Text) {
    range.setStart(text, from);
    if (from === to) range.collapse(true);
    else range.setEnd(text, to);
  } else if (from === to) {
    const part = partAt(text, from);
    range.setStart(part.node, from - part.start);
    range.collapse(true);
  } else {
    const bounds = partRange(text, from, to);
    range.setStart(bounds.startContainer, bounds.startOffset);
    range.setEnd(bounds.endContainer, bounds.endOffset);
  }
  return range;
}

export function patchPaintRange(text: PatchText, from: number, to: number): AbstractRange {
  if (typeof StaticRange !== "function") return setPatchTextRange(new Range(), text, from, to);
  const bounds =
    text instanceof Text
      ? { startContainer: text, endContainer: text, startOffset: from, endOffset: to }
      : partRange(text, from, to);
  return new StaticRange(bounds);
}

function partRange(parts: readonly TextPart[], from: number, to: number): StaticRangeInit {
  const start = partAt(parts, from);
  // An exclusive end on a seam belongs to the preceding node.
  const end = partAt(parts, Math.max(from, to - 1));
  return {
    startContainer: start.node,
    endContainer: end.node,
    startOffset: from - start.start,
    endOffset: to - end.start,
  };
}

function partAt(parts: readonly TextPart[], offset: number) {
  for (let index = parts.length - 1; index > 0; index--) {
    const part = parts[index]!;
    if (part.start <= offset) return part;
  }
  return parts[0]!;
}
