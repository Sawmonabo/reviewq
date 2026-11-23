import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { reportTable } from "../../../api/reports";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { Loading, Notice } from "../../../components/PanelState";
import { MarkdownTree } from "../../../components/markdown/MarkdownContent";
import { parseMarkdown } from "../../../components/markdown/parse-markdown";
import { tablePageTree } from "../../../components/markdown/table-page";
import { useReadingInset } from "../../../hooks/useReadingInset";
import { useResource } from "../../../hooks/useResource";
import { annotate, selectionSource } from "./report-presentation";
import { finishReading } from "./report-position";
import { useVisibleContent } from "./useVisibleContent";

export function ReportTable({
  historyAccess,
  runId,
  report,
  base,
  fallback,
  opening,
  onSource,
  onQuote,
  onRendered,
}: {
  readonly historyAccess: ReactNode;
  readonly runId: string;
  readonly report: ReportSummary;
  readonly base: string | undefined;
  readonly fallback: ReactNode;
  readonly opening: number;
  readonly onSource: (offset: number, byte: number) => void;
  readonly onQuote: (text: string, offset: number) => void;
  readonly onRendered: (receiptId: string) => void;
}) {
  const [offset, setOffset] = useState(opening);
  const [notice, setNotice] = useState<string>();
  const anchor = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const result = useResource(
    `table:${runId}:${report.receiptId}:${report.sha256}:${offset}`,
    async (signal) => {
      const { data, cursor } = await reportTable(runId, report, offset, signal);
      const { range, tableStart, tableEnd } = data;
      if (!range || tableStart === null || tableEnd === null) return { data: null, cursor };
      const table = range.content.kind === "table" ? range.content.page : undefined;
      const markdown =
        range.content.kind === "table" ? range.content.page.markdown : range.content.markdown;
      const native = table ? tablePageTree(table, base) : parseMarkdown(markdown, base);
      if (!native) return { data: null, cursor };
      await finishReading(content.current, signal);
      signal.throwIfAborted();
      const tree = { ...native, children: annotate(native.children) };
      return { data: { range, tableStart, tableEnd, table, markdown, tree }, cursor };
    },
  );
  const [shown, setShown] = useState(result.data);
  const remember = useReadingInset(anchor, shown);
  useEffect(() => {
    if (result.data === undefined) return;
    const frame = requestAnimationFrame(() => {
      remember(anchor.current);
      setShown(result.data);
    });
    return () => cancelAnimationFrame(frame);
  }, [result.data, remember]);
  useVisibleContent(content, shown ? report.receiptId : undefined, onRendered);

  if (shown === null || (!shown && result.error)) return fallback;
  if (!shown)
    return (
      <>
        {historyAccess}
        <Loading label="Reading table rows…" />
      </>
    );
  const { range, tree, table, tableStart, tableEnd, markdown } = shown;
  const introduction = !table && range.byteEnd === tableStart;

  function move(next: number | null) {
    if (next === null) return;
    setNotice(undefined);
    setOffset(next);
  }

  function quote() {
    const selection = getSelection();
    const active = selection?.rangeCount ? selection.getRangeAt(0) : undefined;
    if (!active || !content.current?.contains(active.commonAncestorContainer)) {
      setNotice("Select a passage in this page first.");
      return;
    }
    const exact = selectionSource(active, [
      {
        block: { sourceOrder: 0, offset: 0, end: markdown.length, nodes: [] },
        source: { text: markdown, utf16: 0, scalar: range.scalarStart },
      },
    ]);
    if (exact) {
      setNotice(undefined);
      onQuote(exact.text, exact.offset);
    } else {
      setNotice("Select original text or read the original Markdown to attach a quote.");
    }
  }

  return (
    <section className="stack" aria-label="Report pages">
      <div ref={anchor} className="actions source-nav" aria-busy={result.pending}>
        {table && (
          <span className="meta">
            Rows {table.firstRow + 1}–{table.firstRow + table.rowCount} of {table.totalRows}
          </span>
        )}
        {!introduction && range.byteStart > tableStart && (
          <button type="button" onClick={() => move(tableStart)}>
            First rows
          </button>
        )}
        {range.previousByte !== null && (
          <button type="button" onClick={() => move(range.previousByte)}>
            {range.previousByte < tableStart ? "Read introduction" : "Previous rows"}
          </button>
        )}
        {range.nextByte !== null && (
          <button type="button" onClick={() => move(range.nextByte)}>
            {introduction
              ? "Read table rows"
              : range.nextByte >= tableEnd
                ? "Continue after table"
                : "Next rows"}
          </button>
        )}
        {range.byteEnd < tableEnd && (
          <button type="button" onClick={() => move(tableEnd - 1)}>
            Last rows
          </button>
        )}
        <button type="button" onClick={() => onSource(range.scalarStart, range.byteStart)}>
          Read original source
        </button>
        <button type="button" onMouseDown={(event) => event.preventDefault()} onClick={quote}>
          Quote selection
        </button>
        {historyAccess}
      </div>
      {notice && <Notice>{notice}</Notice>}
      {result.error && <Notice retry={result.retry}>{result.error}</Notice>}
      <div ref={content}>
        <MarkdownTree tree={tree} />
      </div>
    </section>
  );
}
