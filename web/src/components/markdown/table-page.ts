import type { Element, Root, RootContent } from "hast";
import type { TablePage } from "../../api/generated/contracts/common";
import { shiftNodes } from "./live/positions";
import { parseMarkdown } from "./parse-markdown";
import type { MarkdownOptions } from "./parse-markdown";

export function tablePageTree(
  page: TablePage,
  base?: string,
  options?: MarkdownOptions,
): Root | undefined {
  const input = page.headerInSource ? page.markdown : page.header + page.markdown;
  const result = parseMarkdown(input, base, options);
  const table = result?.children[0];
  if (result?.children.length !== 1 || table?.type !== "element" || table.tagName !== "table")
    return;
  const head = child(table, "thead");
  const body = child(table, "tbody");
  const rows = body?.children.filter((row) => row.type === "element" && row.tagName === "tr");
  if (
    !head ||
    !body ||
    rows?.length !== page.rowCount ||
    child(head, "tr")?.children.filter((cell) => cell.type === "element" && cell.tagName === "th")
      .length !== page.widths.length
  )
    return;

  if (!page.headerInSource) {
    forgetPositions(head);
    shiftNodes([body], { offset: -page.header.length, line: -2 });
    delete table.position;
  }

  table.properties.className = ["paged-markdown-table"];
  const total = page.widths.reduce((sum, width) => sum + width, 0);
  table.properties.style = `width: max(100%, ${total}ch)`;
  table.children.unshift({
    type: "element",
    tagName: "colgroup",
    properties: {},
    children: page.widths.map((width) => ({
      type: "element",
      tagName: "col",
      children: [],
      properties: { style: `width: ${(width / total) * 100}%` },
    })),
  });
  return result;
}

function child(element: Element, tag: string) {
  return element.children.find(
    (node): node is Element => node.type === "element" && node.tagName === tag,
  );
}

function forgetPositions(node: RootContent) {
  delete node.position;
  if (node.type === "element") node.children.forEach(forgetPositions);
}
