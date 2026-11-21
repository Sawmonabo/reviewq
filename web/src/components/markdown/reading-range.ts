import type { Root, RootContent } from "hast";

type ReaderOccurrence = {
  readonly startUtf16: number;
  readonly endUtf16: number;
  readonly ordinal: number;
  readonly matched: string;
  readonly markdown: boolean;
};

type Piece = { readonly value: string; readonly node?: Text };
type Display<T> =
  | { readonly text: string; readonly node?: Text }
  | { readonly tag: string; readonly children: readonly T[] };

function pieces<T>(root: T, describe: (item: T) => Display<T> | undefined): Piece[] {
  const result: Piece[] = [];
  function visit(item: T) {
    const display = describe(item);
    if (!display) return;
    if ("text" in display) {
      result.push({ value: display.text, ...(display.node ? { node: display.node } : {}) });
      return;
    }
    if (display.tag === "tr") {
      let first = true;
      for (const child of display.children) {
        const cell = describe(child);
        if (!cell || !("tag" in cell) || (cell.tag !== "td" && cell.tag !== "th")) continue;
        if (!first) result.push({ value: "\t" });
        first = false;
        visit(child);
      }
      return;
    }
    for (const child of display.children) visit(child);
  }
  visit(root);
  return result;
}

export function markdownText(root: Root) {
  return pieces<Root | RootContent>(root, (node) => {
    if (node.type === "root") return { tag: "root", children: node.children };
    if (node.type === "text") return { text: node.value };
    if (node.type !== "element") return;
    if (
      node.tagName === "pre" &&
      node.children.some(
        (child) =>
          child.type === "element" &&
          Array.isArray(child.properties.className) &&
          child.properties.className.includes("language-mermaid"),
      )
    )
      return;
    return { tag: node.tagName, children: node.children };
  })
    .map(({ value }) => value)
    .join("");
}

export function positions(text: string, match: ReaderOccurrence) {
  const { startUtf16, endUtf16, matched, ordinal } = match;
  if (!match.markdown)
    return text.slice(startUtf16, endUtf16) === matched
      ? ([startUtf16, endUtf16] as const)
      : undefined;
  let index = -1;
  let after = 0;
  for (let seen = 0; seen <= ordinal; seen++) {
    index = text.indexOf(matched, after);
    if (index < 0) return;
    after = index + 1;
  }
  return [index, index + matched.length] as const;
}

export function nativeRange(body: HTMLElement, match: ReaderOccurrence) {
  const text = pieces<Node>(body, (node) => {
    if (node instanceof Text) return { text: node.data, node };
    if (
      !(node instanceof Element) ||
      node.matches("button,svg,style,script,[hidden],.mermaid-figure")
    )
      return;
    return { tag: node.localName, children: [...node.childNodes] };
  });
  const span = positions(text.map(({ value }) => value).join(""), match);
  if (!span) return;
  const [start, end] = span;
  let base = 0;
  let range: Range | undefined;
  let last: Text | undefined;
  for (const { value, node } of text) {
    const limit = base + value.length;
    if (node && !range && start < limit && base < end) {
      range = document.createRange();
      range.setStart(node, Math.max(0, start - base));
    }
    if (node && range) {
      if (base >= end && last) {
        range.setEnd(last, last.length);
        return range;
      }
      last = node;
      if (end <= limit) {
        range.setEnd(node, end - base);
        return range;
      }
    }
    base = limit;
  }
}

export function revealDetails(node: Node, body: HTMLElement) {
  let current = node.parentElement;
  while (current && current !== body) {
    if (current instanceof HTMLDetailsElement && !current.open) {
      const summary = current.querySelector(":scope > summary");
      if (!summary?.contains(node)) current.open = true;
    }
    current = current.parentElement;
  }
}

export function revealAcross(range: Range, body: HTMLElement): boolean {
  let region =
    range.commonAncestorContainer instanceof HTMLElement
      ? range.commonAncestorContainer
      : range.commonAncestorContainer.parentElement;
  while (region && body.contains(region)) {
    const overflow = getComputedStyle(region).overflowX;
    if (region.scrollWidth > region.clientWidth && (overflow === "auto" || overflow === "scroll")) {
      const viewport = region.getBoundingClientRect();
      const left = viewport.left + region.clientLeft;
      const right = left + region.clientWidth;
      const place = range.getBoundingClientRect();
      if (place.left < left || place.right > right) {
        region.scrollLeft +=
          place.width >= region.clientWidth || place.left < left
            ? place.left - left
            : place.right - right;
      }
      const visible = range.getBoundingClientRect();
      if (
        visible.left < left - 1 ||
        visible.left >= right ||
        (visible.width <= region.clientWidth && visible.right > right + 1)
      )
        return false;
    }
    region = region.parentElement;
  }
  return true;
}
