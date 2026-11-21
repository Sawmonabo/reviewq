import type { Element, Root } from "hast";
import { MAX_MARKDOWN_NODES, prepareMarkdown, type PreparedMarkdown } from "../parse-markdown";
import { NEWLINE, shiftNodes, type PositionDelta } from "./positions";

type Rows = { readonly items: readonly Element[]; readonly nodes: number };
type TableUpdate = { readonly table?: LiveTable; readonly tree: Root | undefined };

export type LiveTable = {
  readonly heading: PreparedMarkdown;
  readonly table: Element;
  readonly headerSource: string;
  readonly body: PositionDelta;
  readonly next: PositionDelta;
  readonly rows: Rows;
};

const NO_ROWS: Rows = { items: [], nodes: 0 };
const HEADER = /(?:^|\n\n)(\|[^\n]*\n\|[ \t:|-]+\n)/;

export function appendTable(
  source: string,
  base: string | undefined,
  previous: LiveTable | undefined,
): TableUpdate | undefined {
  const table = previous ?? startTable(source, base);
  if (!table) return;
  const pending = source.slice(table.next.offset);
  // Leading pipes make row membership independent of the following line. A blank
  // line, loose row or new block returns the entire document to the canonical path.
  const lines = pending.split("\n");
  const open = lines.pop() ?? "";
  if (lines.some((line) => !line.startsWith("|")) || (open && !open.startsWith("|"))) return;

  const end = pending.length - open.length;
  const complete = readRows(table, pending.slice(0, end), lines.length, table.next, base);
  if (!complete) return;
  const next = { offset: table.next.offset + end, line: table.next.line + lines.length };
  const tail = readRows(table, open, open ? 1 : 0, next, base);
  if (!tail) return;

  const nodes = table.rows.nodes + complete.nodes + tail.nodes;
  // The body, its leading newline and the table's final separator appear once.
  if (table.heading.nodes + nodes + (nodes ? 3 : 0) > MAX_MARKDOWN_NODES)
    return { tree: undefined };
  const rows = complete.items.length
    ? { items: [...table.rows.items, ...complete.items], nodes: table.rows.nodes + complete.nodes }
    : table.rows;
  const tree = withRows(table, [...rows.items, ...tail.items]);
  return tree && { table: { ...table, next, rows }, tree };
}

function startTable(source: string, base: string | undefined): LiveTable | undefined {
  const match = HEADER.exec(source);
  const headerSource = match?.[1];
  if (!match || !headerSource) return;
  const offset = match.index + match[0].length;
  const heading = prepareMarkdown(source.slice(0, offset), base);
  const table = heading?.tree.children.at(-1);
  if (
    !heading ||
    table?.type !== "element" ||
    table.tagName !== "table" ||
    table.position?.start.offset !== offset - headerSource.length ||
    table.position.end.line !== table.position.start.line + 1
  )
    return;
  const body = { offset, line: table.position.end.line + 1 };
  return { heading, table, headerSource, body, next: body, rows: NO_ROWS };
}

function readRows(
  table: LiveTable,
  source: string,
  count: number,
  start: PositionDelta,
  base: string | undefined,
): Rows | undefined {
  if (!count) return NO_ROWS;
  const parsed = prepareMarkdown(table.headerSource + source, base)?.tree;
  const node = parsed?.children[0];
  if (parsed?.children.length !== 1 || node?.type !== "element" || node.tagName !== "table") return;
  const body = node.children.find((child) => child.type === "element" && child.tagName === "tbody");
  if (body?.type !== "element") return;
  const items = body.children.filter(
    (child): child is Element => child.type === "element" && child.tagName === "tr",
  );
  if (items.length !== count || body.position?.start.offset !== table.headerSource.length) return;
  const nodes = shiftNodes(items, {
    line: start.line - 3,
    offset: start.offset - table.headerSource.length,
  });
  return { items, nodes: nodes + count }; // Each row is followed by one generated newline.
}

function withRows(state: LiveTable, rows: readonly Element[]): Root | undefined {
  if (!rows.length) return state.heading.tree;
  const children: Element["children"] = [NEWLINE];
  for (const row of rows) children.push(row, NEWLINE);
  const start = rows[0]?.position?.start;
  const end = rows.at(-1)?.position?.end;
  const tableStart = state.table.position?.start;
  if (!start || !end || !tableStart || start.offset !== state.body.offset) return;
  const body: Element = {
    type: "element",
    tagName: "tbody",
    properties: {},
    position: { start, end },
    children,
  };
  return {
    ...state.heading.tree,
    children: [
      ...state.heading.tree.children.slice(0, -1),
      {
        ...state.table,
        position: { start: tableStart, end },
        children: [...state.table.children, body, NEWLINE],
      },
    ],
  };
}
