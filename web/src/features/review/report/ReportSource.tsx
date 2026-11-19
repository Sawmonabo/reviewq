import { useRef, useState } from "react";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { reportSection } from "../../../api/reports";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";

const STEP = 12_000;

export function ReportSource({ runId, report, requestedOffset, reason, onQuote }: {
  readonly runId: string;
  readonly report: ReportSummary;
  readonly requestedOffset: number;
  readonly reason: string;
  readonly onQuote: (text: string, offset: number) => void;
}) {
  const [seek, setSeek] = useState<{ request: number; offset: number }>();
  const [entry, setEntry] = useState("");
  const [problem, setProblem] = useState<string>();
  const block = useRef<HTMLPreElement>(null);
  const offset = seek?.request === requestedOffset ? seek.offset : requestedOffset;
  const source = useResource(`source:${runId}:${report.receiptId}:${offset}`, (signal) =>
    reportSection(runId, report.receiptId, report.sha256, offset, signal));
  const value = source.data;

  function move(next: number) {
    if (!Number.isSafeInteger(next) || next < 0 || next > Math.max(0, report.totalCodePoints - 1)) {
      setProblem("Enter a source offset within the document.");
      return;
    }
    setSeek({ request: requestedOffset, offset: next });
    setProblem(undefined);
  }

  function capture() {
    const selection = getSelection();
    const host = block.current;
    if (!host || !value || !selection || !selection.rangeCount || selection.isCollapsed) {
      setProblem("Select text in this source window first.");
      return;
    }
    const selected = selection.getRangeAt(0);
    if (!host.contains(selected.startContainer) || !host.contains(selected.endContainer)) {
      setProblem("Select a passage within this source window.");
      return;
    }
    const before = document.createRange();
    before.selectNodeContents(host);
    before.setEnd(selected.startContainer, selected.startOffset);
    if (value.text.slice(before.toString().length, before.toString().length + selected.toString().length) !== selected.toString()) {
      setProblem("The selection must match the original Markdown.");
      return;
    }
    setProblem(undefined);
    onQuote(selected.toString(), value.offset + [...before.toString()].length);
  }

  return <section className="surface-card stack" aria-label="Report source window">
    <div className="spread"><h3>{reason}</h3>{value && <span className="meta">Characters {value.offset + 1}–{value.nextOffset ?? value.totalCodePoints} of {value.totalCodePoints}</span>}</div>
    <div className="actions source-nav">
      <button type="button" disabled={offset === 0} onClick={() => move(0)}>Start</button>
      <button type="button" disabled={offset === 0} onClick={() => move(Math.max(0, offset - STEP))}>Previous</button>
      <button type="button" disabled={value?.nextOffset == null} onClick={() => { if (value?.nextOffset != null) move(value.nextOffset); }}>Next</button>
      <button type="button" disabled={offset >= report.totalCodePoints - STEP} onClick={() => move(Math.max(0, report.totalCodePoints - STEP))}>End</button>
      <button type="button" disabled={!value} onClick={capture}>Quote selection</button>
    </div>
    <form className="actions" onSubmit={(event) => { event.preventDefault(); move(Number(entry)); }}>
      <label>Source offset <input type="number" min={0} max={report.totalCodePoints} value={entry} onChange={(event) => setEntry(event.currentTarget.value)} /></label>
      <button type="submit" disabled={!entry}>Go to offset</button>
    </form>
    <p className="meta">Browser Find searches this source window.</p>
    {problem && <Notice>{problem}</Notice>}
    {source.error && <Notice retry={source.retry}>{source.error}</Notice>}
    {source.pending && <Loading label="Reading original Markdown…" />}
    {value && <pre ref={block} className="report-source" tabIndex={0}>{value.text}</pre>}
  </section>;
}
