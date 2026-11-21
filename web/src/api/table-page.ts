import type { TablePage } from "./generated/contracts/common";

const utf8 = new TextEncoder();

export function validateTablePage(page: TablePage, sourceBytes: number) {
  if (
    page.rowCount < 1 ||
    page.rowCount > 48 ||
    page.firstRow + page.rowCount > page.totalRows ||
    page.headerInSource !== (page.firstRow === 0) ||
    page.widths.some((width) => width < 8 || width > 56) ||
    utf8.encode(page.header).length > 4 * 1024 ||
    !page.header.endsWith("\n") ||
    page.header.split("\n").length !== 3 ||
    sourceBytes > 36 * 1024 ||
    utf8.encode(page.markdown).length !== sourceBytes ||
    (page.headerInSource && !page.markdown.startsWith(page.header))
  )
    throw new Error("The original table range is invalid.");
}
