import type { Root, RootContent } from "hast";
import type {
  HistoryContent,
  HistoryPageResponse,
  HistoryPart,
} from "../../../../api/generated/contracts";
import type {
  HistorySearchDestination,
  HistorySearchHit,
} from "../../../../api/generated/contracts/history";
import {
  historyContent,
  historyPage,
  historyParts,
  historySearchDestination,
} from "../../../../api/history";
import type { SessionAddress } from "../../../../api/history";
import { parseMarkdown } from "../../../../components/markdown/parse-markdown";

const changed = "This occurrence is no longer available in the conversation.";
const highlightName = "reviewq-transcript-match";

export type FoundOutput = {
  readonly destination: HistorySearchDestination;
  readonly content: HistoryContent;
  readonly tree?: Root;
};

export type PreparedDestination = {
  readonly page: HistoryPageResponse;
  readonly output: FoundOutput;
  readonly extra?: { readonly items: readonly HistoryPart[]; readonly next: string | null };
};

export async function prepareDestination(
  target: SessionAddress,
  hit: HistorySearchHit,
  signal: AbortSignal,
): Promise<PreparedDestination> {
  const destination = await historySearchDestination(target, hit.hit, signal);
  if (destination.entryId !== hit.entryId || destination.matched !== hit.matched)
    throw new Error(changed);
  const { entryId, generation, locator, contentRef, contentCursor, nativeIndex, partsCursor } =
    destination;
  const [page, content] = await Promise.all([
    historyPage(target, { aroundId: entryId, locator }, signal),
    historyContent(target, entryId, generation, contentRef, contentCursor ?? undefined, signal),
  ]);
  const entry = page.data.items.find((row) => row.entryId === entryId);
  if (
    !entry ||
    page.data.generation !== generation ||
    page.data.reset ||
    content.offset !== destination.offset
  )
    throw new Error(changed);
  const tree = destination.markdown ? parseMarkdown(content.text) : undefined;
  if (destination.markdown && (!tree || !content.complete)) throw new Error(changed);
  if (!positions(tree ? markdownText(tree) : content.text, destination)) throw new Error(changed);
  let extra;
  if (nativeIndex !== null && !entry.parts.some((part) => part.nativeIndex === nativeIndex)) {
    if (!partsCursor) throw new Error(changed);
    const result = await historyParts(target, entryId, partsCursor, signal);
    if (!result.parts.some((part) => part.nativeIndex === nativeIndex)) throw new Error(changed);
    extra = { items: result.parts, next: result.nextCursor };
  }
  signal.throwIfAborted();
  // The read can race an in-place replacement even when native history keeps the same generation.
  await historySearchDestination(target, hit.hit, signal);
  return {
    page,
    output: { destination, content, ...(tree ? { tree } : {}) },
    ...(extra ? { extra } : {}),
  };
}

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

function markdownText(root: Root) {
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

function positions(text: string, match: HistorySearchDestination) {
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

function nativeRange(body: HTMLElement, match: HistorySearchDestination) {
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

function revealDetails(node: Node, body: HTMLElement) {
  let current = node.parentElement;
  while (current && current !== body) {
    if (current instanceof HTMLDetailsElement && !current.open) {
      const summary = current.querySelector(":scope > summary");
      if (!summary?.contains(node)) current.open = true;
    }
    current = current.parentElement;
  }
}

export function revealOccurrence(view: HTMLElement, match: HistorySearchDestination) {
  const row = view.querySelector('[data-native-entry="' + CSS.escape(match.entryId) + '"]');
  const body = row?.querySelector<HTMLElement>("[data-find-body]");
  const range = body && nativeRange(body, match);
  if (!body || !range) return false;
  revealDetails(range.startContainer, body);
  revealDetails(range.endContainer, body);
  const rect = range.getBoundingClientRect();
  if (!rect.height) return false;
  const bounds = view.getBoundingClientRect();
  if (rect.top < bounds.top + 32 || rect.bottom > bounds.bottom - 12) {
    view.scrollTop += rect.top - bounds.top - bounds.height * 0.32;
  }
  const visible = range.getBoundingClientRect();
  if (
    visible.top < bounds.top - 1 ||
    visible.top >= bounds.bottom ||
    (visible.height <= bounds.height && visible.bottom > bounds.bottom + 1)
  )
    return false;
  if (typeof CSS.highlights !== "undefined" && typeof Highlight !== "undefined") {
    CSS.highlights.set(highlightName, new Highlight(range));
  }
  return true;
}

export function clearOccurrence() {
  CSS.highlights?.delete(highlightName);
}
