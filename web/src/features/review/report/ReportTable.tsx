import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { reportTable } from "../../../api/reports";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { Loading, Notice } from "../../../components/PanelState";
import { MarkdownTree } from "../../../components/markdown/MarkdownContent";
import { tablePageTree } from "../../../components/markdown/table-page";
import { useReadingInset } from "../../../hooks/useReadingInset";
import { useResource } from "../../../hooks/useResource";
import { annotate, selectionSource } from "./report-presentation";
import { finishReading } from "./report-position";
import { useVisibleContent } from "./useVisibleContent";

export function ReportTable({
  runId,
  report,
  base,
  fallback,
  opening,
  onSource,
  onQuote,
  onRendered,
}: {
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
      const range = data.range;
      if (!range) return { data: null, cursor };
      const native = tablePageTree(range.table, base);
      if (!native) return { data: null, cursor };
      await finishReading(content.current, signal);
      signal.throwIfAborted();
      const tree = { ...native, children: annotate(native.children) };
      return { data: { range, tree }, cursor };
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
  if (!shown) return <Loading label="Reading table rows…" />;
  const { range, tree } = shown;
  const { table } = range;

  function move(next: number | null) {
    if (next === null) return;
    setNotice(undefined);
    setOffset(next);
  }

  function quote() {
    const selection = getSelection();
    const active = selection?.rangeCount ? selection.getRangeAt(0) : undefined;
    if (!active || !content.current?.contains(active.commonAncestorContainer)) {
      setNotice("Select a passage in the table first.");
      return;
    }
    const exact = selectionSource(active, [
      {
        block: { sourceOrder: 0, offset: 0, end: table.markdown.length, nodes: [] },
        source: { text: table.markdown, utf16: 0, scalar: range.scalarStart },
      },
    ]);
    if (exact) {
      setNotice(undefined);
      onQuote(exact.text, exact.offset);
    } else {
      setNotice("Select original rows or read the original Markdown to attach a quote.");
    }
  }

  return (
    <section className="stack" aria-label="Report table rows">
      <div ref={anchor} className="actions source-nav" aria-busy={result.pending}>
        <span className="meta">
          Rows {table.firstRow + 1}–{table.firstRow + table.rowCount} of {table.totalRows}
        </span>
        <button type="button" disabled={range.previousByte === null} onClick={() => move(0)}>
          First rows
        </button>
        <button
          type="button"
          disabled={range.previousByte === null}
          onClick={() => move(range.previousByte)}
        >
          Previous rows
        </button>
        <button
          type="button"
          disabled={range.nextByte === null}
          onClick={() => move(range.nextByte)}
        >
          Next rows
        </button>
        <button
          type="button"
          disabled={range.nextByte === null}
          onClick={() => move(report.byteLength)}
        >
          Last rows
        </button>
        <button type="button" onClick={() => onSource(range.scalarStart, range.byteStart)}>
          Read original source
        </button>
        <button type="button" onMouseDown={(event) => event.preventDefault()} onClick={quote}>
          Quote selection
        </button>
      </div>
      {notice && <Notice>{notice}</Notice>}
      {result.error && <Notice retry={result.retry}>{result.error}</Notice>}
      <div ref={content}>
        <MarkdownTree tree={tree} />
      </div>
    </section>
  );
}
