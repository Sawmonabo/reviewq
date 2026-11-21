import type { Root, RootContent } from "hast";
import { stableBoundary } from "../boundary/split";
import {
  admitMarkdown,
  MAX_MARKDOWN_NODES,
  prepareMarkdown,
  type PreparedMarkdown,
} from "../parse-markdown";
import { NEWLINE, shiftNodes, type PositionDelta } from "./positions";
import { appendTable, type LiveTable } from "./table";

type Sealed = PreparedMarkdown & { readonly end: PositionDelta };

export type LiveMarkdown = {
  readonly source: string;
  readonly base: string | undefined;
  readonly tree: Root | undefined;
  readonly blocks?: Sealed;
  readonly table?: LiveTable;
};

const MAX_LIVE_SOURCE = 64 * 1024;
// A mid-document BOM only looks like a document signature when parsed in isolation.
const WHOLE_DOCUMENT = /[[<\r\uFEFF]/;
const CONTAINER = /^[ \t>]|^ {0,3}[-=]+[ \t]*$/m;

export function prepareLiveMarkdown(
  source: string,
  base: string | undefined,
  previous: LiveMarkdown | undefined,
): LiveMarkdown {
  const state = { source, base };
  if (!admitMarkdown(source)) return { ...state, tree: undefined };
  const append =
    previous && base === previous.base && source.startsWith(previous.source) ? previous : undefined;
  if (source.length <= MAX_LIVE_SOURCE && !WHOLE_DOCUMENT.test(source)) {
    if (source.includes("|")) {
      const result = appendTable(source, base, append?.table);
      if (result) return { ...state, ...result };
    } else if (!CONTAINER.test(source)) {
      const result = appendBlocks(source, base, append?.blocks);
      if (result) return { ...state, ...result };
    }
  }
  return { ...state, tree: prepareMarkdown(source, base)?.tree };
}

function appendBlocks(
  source: string,
  base: string | undefined,
  sealed: Sealed | undefined,
): Pick<LiveMarkdown, "tree" | "blocks"> | undefined {
  const origin = sealed?.end ?? { offset: 0, line: 0 };
  const pending = source.slice(origin.offset);
  const provisional = prepareMarkdown(pending, base);
  if (!provisional) return;
  if (
    sealed &&
    provisional.tree.children.length &&
    sealed.nodes + provisional.nodes > MAX_MARKDOWN_NODES
  )
    return { tree: undefined };

  const { children } = provisional.tree;
  const next = confirmedBoundary(pending, children);
  if (!next) {
    shiftNodes(children, origin);
    return { tree: combine(sealed?.tree, provisional.tree), ...(sealed && { blocks: sealed }) };
  }

  const ahead = children[next.index];
  const point = ahead?.position?.start;
  if (point?.offset === undefined) return;
  const finished = children.slice(0, next.index - 1);
  const tail = children.slice(next.index);
  const nodes = shiftNodes(finished, origin) + 1;
  shiftNodes(tail, origin);

  const blocks = {
    tree: combine(sealed?.tree, { ...provisional.tree, children: finished }),
    nodes: (sealed?.nodes ?? 0) + nodes,
    end: { offset: origin.offset + point.offset, line: origin.line + point.line - 1 },
  };
  return { tree: combine(blocks.tree, { ...provisional.tree, children: tail }), blocks };
}

function confirmedBoundary(source: string, children: readonly RootContent[]) {
  if (children.length < 3) return;
  const proposed = stableBoundary(source);
  // A detector hint must agree with a top-level boundary from the actual parser.
  // The following complete line witnesses a separation that more input cannot undo.
  for (let index = children.length - 1; index >= 2; index--) {
    const next = children[index]?.position?.start;
    const between = children[index - 1];
    const before = children[index - 2]?.position?.end;
    if (
      next?.offset !== undefined &&
      next.column === 1 &&
      next.offset <= proposed &&
      source.slice(next.offset - 2, next.offset) === "\n\n" &&
      source.indexOf("\n", next.offset) >= 0 &&
      before?.offset !== undefined &&
      before.offset < next.offset &&
      between?.type === "text" &&
      between.value === "\n" &&
      !between.position
    )
      return { index };
  }
}

function combine(sealed: Root | undefined, tail: Root): Root {
  if (!sealed?.children.length) return tail;
  if (!tail.children.length) return sealed;
  return { ...sealed, children: [...sealed.children, NEWLINE, ...tail.children] };
}
