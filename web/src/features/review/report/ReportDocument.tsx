import { useMemo, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { QuoteRef, ReportSummary } from "../../../api/generated/contracts/reports";
import { reportFeedback, reportView } from "../../../api/reports";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { parseMarkdown } from "../../../components/markdown/parse-markdown";
import { useDraft } from "../../../hooks/useDraft";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { safeUrl } from "../../../lib/presentation";
import { Composer } from "../Composer";
import { ReportDetails } from "./ReportDetails";
import { ReportSource } from "./ReportSource";
import { RichReport } from "./RichReport";

type Quote = { readonly text: string; readonly reference: QuoteRef };
type Props = {
  readonly run: RunDetail;
  readonly report: ReportSummary;
  readonly current: boolean;
  readonly drafts: DraftStore;
  readonly reviewTitle: string;
  readonly webUrl: string;
};

export default function ReportDocument({ run, report, current, drafts, reviewTitle, webUrl }: Props) {
  const [source, setSource] = useState(false);
  const [sourceOffset, setSourceOffset] = useState(0);
  const [notice, setNotice] = useState<string>();
  const [context, setContext] = useState<"findings" | "changes" | "sections">("findings");
  const quoteKey = JSON.stringify(["quote", run.reviewId, run.runId, run.leadSessionKey, report.receiptId, report.sha256]);
  const quoteDraft = useDraft(drafts, quoteKey);
  const quote = readQuote(quoteDraft);
  const admitted = report.semanticIndex === "ready" && report.byteLength <= 1024 * 1024 && report.lineCount <= 10_000 && report.maxLineBytes <= 16_384;
  const body = useResource(`report-body:${run.runId}:${report.receiptId}:${source || !admitted}`, (signal) =>
    source || !admitted ? Promise.resolve({ data: null, cursor: "" }) : reportView(run.runId, signal, report.receiptId, "markdown"));
  const markdown = body.data?.report?.sha256 === report.sha256 ? body.data?.markdown : undefined;
  const base = safeUrl(webUrl);
  const tree = useMemo(() => {
    if (typeof markdown !== "string") return;
    try { return parseMarkdown(markdown, base); } catch { return undefined; }
  }, [markdown, base]);
  const formattingFailure = Boolean(typeof markdown === "string" && !tree);
  const sourceMode = source || !admitted || formattingFailure || Boolean(body.error);
  const target = { runId: run.runId, sessionKey: run.leadSessionKey };

  function capture(text: string, offset: number) {
    const length = [...text].length;
    if (!text.trim() || length > 12_000) {
      setNotice("Select a non-empty quote up to 12,000 characters.");
      return;
    }
    const next: Quote = { text, reference: { offset, length, unit: "unicode_codepoints" } };
    setNotice(drafts.set(quoteKey, `Quote · ${reviewTitle}`, JSON.stringify(next))
      ? "Quote attached to feedback." : "Draft storage full. Free space before attaching the quote.");
  }

  function openSource(offset = 0) {
    setSourceOffset(offset);
    setSource(true);
  }

  return <>
    <div className="spread">
      <div className="actions">
        {admitted && <button type="button" onClick={() => source ? setSource(false) : openSource()}>{source ? "Read formatted report" : "Read original source"}</button>}
        {report.findingCount !== null && <span className="meta">{report.findingCount} findings</span>}
        {report.changeCount !== null && <span className="meta">{report.changeCount} recorded changes</span>}
      </div>
      <span className="meta" title={report.sha256}>SHA-256 <code>{report.sha256.slice(0, 12)}</code></span>
    </div>
    {notice && <Notice>{notice}</Notice>}
    {sourceMode ? <ReportSource runId={run.runId} report={report} requestedOffset={sourceOffset} reason={!admitted
      ? report.semanticIndex === "source_only" ? "Semantic index unavailable · Original source" : "Large report · source view"
      : formattingFailure ? "Report formatting limit · Original source" : body.error ? "Formatted report unavailable · Original source" : "Original Markdown"}
      onQuote={capture} />
      : body.pending ? <Loading label="Loading accepted report…" /> : tree && typeof markdown === "string"
        ? <RichReport tree={tree} source={markdown} onQuote={capture} onSource={(offset) => {
          openSource(offset);
          setNotice("Select the exact passage in the original Markdown to attach a quote.");
        }} /> : <Empty title="Formatted report unavailable"><button type="button" onClick={() => openSource()}>Read original source</button></Empty>}
    {report.semanticIndex === "ready" && <section className="stack" aria-label="Structured review context">
      <div className="actions report-context">
        {(["findings", "changes", "sections"] as const).map((kind) => <button type="button" key={kind} aria-pressed={kind === context}
          onClick={() => setContext(kind)}>{kind === "findings" ? "Finding cards" : kind === "changes" ? "Finding outcomes" : "Source sections"}</button>)}
      </div>
      <ReportDetails key={context} run={run} report={report} collection={context} current={current}
        drafts={drafts} reviewTitle={reviewTitle} onSource={openSource} />
    </section>}
    <section className="surface-card report-feedback" aria-label="Report feedback">
      <h3>Send feedback to the lead</h3>
      {quote && <blockquote className="report-quote"><p>{quote.text}</p><cite>Source offset {quote.reference.offset} · {report.sha256.slice(0, 12)}</cite>
        <button type="button" onClick={() => { drafts.set(quoteKey, `Quote · ${reviewTitle}`, ""); setNotice(undefined); }}>Remove quote</button></blockquote>}
      <Composer store={drafts} target={target} draftKey={JSON.stringify(["feedback", run.reviewId, run.runId, run.leadSessionKey, report.receiptId, report.sha256])}
        label="Write feedback on this report" targetLabel={`To selected lead · report ${report.sha256.slice(0, 12)}`}
        send={(text, submissionId) => reportFeedback(target, { text, submissionId, quote: quote?.text ?? "",
          quoteRef: quote?.reference ?? null, receiptId: report.receiptId, reportHash: report.sha256 })}
        onAccepted={() => { if (drafts.getSnapshot().entries.find((draft) => draft.key === quoteKey)?.text === quoteDraft) {
          drafts.set(quoteKey, `Quote · ${reviewTitle}`, "");
        } }} />
    </section>
  </>;
}

function readQuote(value: string): Quote | undefined {
  if (!value) return;
  try {
    const candidate = JSON.parse(value) as Quote;
    if (candidate.reference.unit === "unicode_codepoints" && Number.isSafeInteger(candidate.reference.offset)
      && candidate.reference.length === [...candidate.text].length) return candidate;
  } catch { /* Memory-only draft may have been discarded. */ }
}
