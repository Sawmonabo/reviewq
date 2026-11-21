import type { Element, ElementContent, Root, RootContent } from "hast";
import type { PreparedTable, TableInline } from "../../api/generated/contracts/common";
import { safeUrl } from "../../lib/presentation";
import { NEWLINE, shiftNodes } from "./live/positions";
import {
  MAX_MARKDOWN_NODES,
  admitMarkdown,
  parseMarkdown,
  prepareMarkdown,
} from "./parse-markdown";

type Piece = { readonly markdown: string; readonly table?: PreparedTable | null | undefined };
type Point = NonNullable<Element["position"]>["start"];

export function tableCost(table: PreparedTable | null | undefined) {
  // The node charge includes the extra source-position spans in Findings.
  return table ? table.encodedBytes + table.nodes * 40 : 0;
}

export function parseSettledMarkdown(
  text: string,
  pieces: readonly Piece[],
  base?: string,
): Root | undefined {
  if (!pieces.some((piece) => piece.table)) return parseMarkdown(text, base);
  if (!admitMarkdown(text)) return;
  const children: RootContent[] = [];
  let count = 1;
  let start = 0;
  let at = 0;
  let line = 0;
  let startLine = 0;

  function append(nodes: RootContent[], length: number) {
    if (!nodes.length) return;
    if (children.length) children.push(NEWLINE);
    children.push(...nodes);
    count += length + 1;
  }

  function flush(end: number) {
    if (start === end) return true;
    const parsed = prepareMarkdown(text.slice(start, end), base);
    if (!parsed) return false;
    shiftNodes(parsed.tree.children, { offset: start, line: startLine });
    append(parsed.tree.children, parsed.nodes - 1);
    return true;
  }

  for (const piece of pieces) {
    const table = piece.table && nativeTable(piece.markdown, piece.table, base);
    const next = at + piece.markdown.length;
    const nextLine = line + piece.markdown.split("\n").length - 1;
    if (table && piece.table) {
      if (!flush(at)) return;
      shiftNodes([table], { offset: at, line });
      append([table], piece.table.nodes);
      start = next;
      startLine = nextLine;
    }
    at = next;
    line = nextLine;
  }
  if (at !== text.length || !flush(at) || count > MAX_MARKDOWN_NODES) return;
  return { type: "root", children };
}

function nativeTable(source: string, packet: PreparedTable, base?: string) {
  if (
    packet.encodedBytes > 96 * 1024 ||
    packet.nodes > 8_000 ||
    packet.start >= packet.end ||
    packet.end > source.length ||
    source.slice(0, packet.start).trim() ||
    source.slice(packet.end).trim()
  )
    return;
  const lines = [0];
  for (let offset = source.indexOf("\n"); offset >= 0; offset = source.indexOf("\n", offset + 1))
    lines.push(offset + 1);
  function point(offset: number): Point {
    let lo = 0;
    let hi = lines.length;
    while (hi - lo > 1) {
      const middle = (hi + lo) >>> 1;
      if (lines[middle]! <= offset) lo = middle;
      else hi = middle;
    }
    return { offset, line: lo + 1, column: offset - lines[lo]! + 1 };
  }
  function position(start: number, end: number) {
    return { start: point(start), end: point(end) };
  }
  const rows: Element[] = [];
  let previous = packet.start;
  let tokenCount = 0;
  for (const row of packet.rows) {
    if (row.length !== packet.columns.length) return;
    const cells: Element[] = [];
    for (const [column, cell] of row.entries()) {
      const align = packet.columns[column];
      const tag = rows.length === 0 ? "th" : "td";
      const target = element(tag, [], align === "default" ? {} : { align });
      const parents = [target];
      for (const token of cell) {
        if (++tokenCount > 8_000) return;
        const parent = parents.at(-1)!;
        if (token.kind === "text" || token.kind === "code") {
          const { start, end } = token;
          if (
            start < previous ||
            end <= start ||
            end > packet.end ||
            splitsScalar(source, start) ||
            splitsScalar(source, end)
          )
            return;
          previous = end;
          const witness = position(start, end);
          if (token.kind === "text")
            parent.children.push({
              type: "text",
              value: source.slice(start, end),
              position: witness,
            });
          else
            parent.children.push({
              ...element("code", [{ type: "text", value: token.text, position: witness }]),
              position: witness,
            });
        } else if (token.kind === "close") {
          if (parents.length === 1) return;
          parents.pop();
        } else {
          const node = openInline(token, base);
          if (!node || parents.length > 16) return;
          parent.children.push(node);
          parents.push(node);
        }
      }
      if (parents.length !== 1) return;
      cells.push(target);
    }
    rows.push(element("tr", withNewlines(cells)));
  }
  if (!rows[0] || rows.length < 2) return;
  return {
    ...element(
      "table",
      withNewlines([
        element("thead", withNewlines([rows[0]])),
        element("tbody", withNewlines(rows.slice(1))),
      ]),
    ),
    position: position(packet.start, packet.end),
  };
}

function openInline(
  token: Exclude<TableInline, { kind: "text" | "code" | "close" }>,
  base?: string,
) {
  switch (token.kind) {
    case "emphasis":
      return element("em", []);
    case "strong":
      return element("strong", []);
    case "strike":
      return element("del", []);
    case "link": {
      const title = token.title ? { title: token.title } : {};
      if (token.href.startsWith("#")) return element("a", [], { href: token.href, ...title });
      const href = safeUrl(token.href, base);
      return href && element("a", [], { href, ...title, target: "_blank", rel: ["noreferrer"] });
    }
  }
}

function element(
  tagName: string,
  children: ElementContent[],
  properties: Element["properties"] = {},
): Element {
  return { type: "element", tagName, properties, children };
}

function withNewlines(elements: readonly Element[]): ElementContent[] {
  return [NEWLINE, ...elements.flatMap((node) => [node, NEWLINE])];
}

function splitsScalar(source: string, offset: number) {
  return (source.charCodeAt(offset) & 0xfc00) === 0xdc00;
}
