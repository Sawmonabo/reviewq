import { useMemo, useRef, useState } from "react";
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

type Quote = { readonly text: string; readonly reference: QuoteRef | null };
type Props = {
  readonly run: RunDetail;
  readonly report: ReportSummary;
  readonly current: boolean;
  readonly drafts: DraftStore;
  readonly reviewTitle: string;
  readonly webUrl: string;
};

export default function ReportDocument({
  run,
  report,
  current,
  drafts,
  reviewTitle,
  webUrl,
}: Props) {
  const [source, setSource] = useState(false);
  const [sourceOffset, setSourceOffset] = useState(0);
  const [notice, setNotice] = useState<string>();
  const [feedbackNotice, setFeedbackNotice] = useState<string>();
  const [contextOpen, setContextOpen] = useState(false);
  const [context, setContext] = useState<"findings" | "changes" | "sections">("findings");
  const feedback = useRef<HTMLElement>(null);
  const reader = useRef<HTMLDivElement>(null);
  const address = [run.reviewId, run.runId, run.leadSessionKey, report.receiptId, report.sha256];
  const quoteKey = JSON.stringify(["quote", ...address]);
  const quoteLabel = `Quote · ${reviewTitle}`;
  const quoteDraft = useDraft(drafts, quoteKey);
  const quote = readQuote(quoteDraft);
  const admitted =
    report.semanticIndex === "ready" &&
    report.byteLength <= 1024 * 1024 &&
    report.lineCount <= 10_000 &&
    report.maxLineBytes <= 16_384;
  const body = useResource(`report-body:${run.runId}:${report.receiptId}:${admitted}`, (signal) =>
    admitted
      ? reportView(run.runId, signal, report.receiptId, "markdown")
      : Promise.resolve({ data: null, cursor: "" }),
  );
  const markdown = body.data?.report?.sha256 === report.sha256 ? body.data?.markdown : undefined;
  const base = safeUrl(webUrl);
  const tree = useMemo(() => {
    if (typeof markdown !== "string") return;
    try {
      return parseMarkdown(markdown, base);
    } catch {
      return undefined;
    }
  }, [markdown, base]);
  const formattingFailure = Boolean(typeof markdown === "string" && !tree);
  const sourceMode = source || !admitted || formattingFailure || Boolean(body.error);
  const target = { runId: run.runId, sessionKey: run.leadSessionKey };

  function focusFeedback() {
    const section = feedback.current;
    if (!section) return;
    getSelection()?.removeAllRanges();
    scrollWithinDrawer(section);
    section
      .querySelector<HTMLTextAreaElement>(".composer textarea")
      ?.focus({ preventScroll: true });
  }

  function writeQuote(next?: Quote) {
    const saved = drafts.set(quoteKey, quoteLabel, next ? JSON.stringify(next) : "");
    if (!saved) setFeedbackNotice("Draft storage full. Free space before editing the quote.");
    return saved;
  }

  function capture(text: string, offset: number, focus = true) {
    const length = [...text].length;
    if (!text.trim() || length > 12_000) {
      setFeedbackNotice("Select a non-empty quote up to 12,000 characters.");
    } else if (writeQuote({ text, reference: { offset, length, unit: "unicode_codepoints" } })) {
      setFeedbackNotice("Passage attached to feedback.");
    }
    if (focus) focusFeedback();
  }

  function verifyPassage() {
    const text = quote?.text;
    if (!text?.trim() || quote?.reference) return;
    const offset = markdown?.indexOf(text) ?? -1;
    if (typeof markdown !== "string" || offset < 0) {
      setFeedbackNotice("Select an exact passage in the report or original source.");
      return;
    }
    if (markdown.indexOf(text, offset + 1) !== -1) {
      setFeedbackNotice(
        "This passage appears more than once. Select the intended occurrence in the report.",
      );
      return;
    }
    capture(text, [...markdown.slice(0, offset)].length, false);
  }

  function openSource(offset = 0) {
    setSourceOffset(offset);
    setSource(true);
    const element = reader.current;
    if (element)
      requestAnimationFrame(() => {
        if (element.isConnected) scrollWithinDrawer(element);
      });
  }

  return (
    <>
      <div className="report-reader" ref={reader}>
        {notice && <Notice>{notice}</Notice>}
        {sourceMode ? (
          <ReportSource
            runId={run.runId}
            report={report}
            requestedOffset={sourceOffset}
            reason={
              !admitted
                ? report.semanticIndex === "source_only"
                  ? "Semantic index unavailable · Original source"
                  : "Large report · source view"
                : formattingFailure
                  ? "Report formatting limit · Original source"
                  : body.error
                    ? "Formatted report unavailable · Original source"
                    : "Original Markdown"
            }
            onQuote={capture}
          />
        ) : body.pending ? (
          <Loading label="Loading accepted report…" />
        ) : tree && typeof markdown === "string" ? (
          <RichReport
            tree={tree}
            source={markdown}
            bookmark={`findings.position.${run.runId}.${report.receiptId}.${report.sha256}`}
            onQuote={capture}
            onSource={(offset) => {
              openSource(offset);
              setNotice("Select the exact passage in the original Markdown to attach a quote.");
            }}
          />
        ) : (
          <Empty title="Formatted report unavailable">
            <button type="button" onClick={() => openSource()}>
              Read original source
            </button>
          </Empty>
        )}
      </div>
      <div className="report-source-tools spread">
        {admitted && (
          <button
            type="button"
            className="quiet-button"
            onClick={() => {
              setNotice(undefined);
              if (source) setSource(false);
              else openSource();
            }}
          >
            {source ? "Read formatted report" : "Read original source"}
          </button>
        )}
        <span className="meta" title={report.sha256}>
          SHA-256 <code>{report.sha256.slice(0, 12)}</code>
        </span>
      </div>
      <section className="report-feedback stack" aria-label="Report feedback" ref={feedback}>
        <label className="field report-quote-field">
          Finding or passage to reference
          <small className="meta">Optional · quote from this report</small>
          <textarea
            rows={2}
            value={quote?.text ?? ""}
            placeholder="Select a finding in the report or paste an exact passage"
            onChange={(event) => {
              setFeedbackNotice(undefined);
              const text = event.currentTarget.value;
              writeQuote(text ? { text, reference: null } : undefined);
            }}
          />
        </label>
        {quote?.text && (
          <div className="actions report-quote-actions">
            {quote.reference ? (
              <span className="meta">Attached from this report</span>
            ) : (
              <button
                type="button"
                className="quiet-button"
                disabled={!quote.text.trim()}
                onClick={verifyPassage}
              >
                Attach passage
              </button>
            )}
            <button
              type="button"
              className="quiet-button"
              onClick={() => {
                setFeedbackNotice(undefined);
                writeQuote();
              }}
            >
              Remove quote
            </button>
          </div>
        )}
        {feedbackNotice && <Notice>{feedbackNotice}</Notice>}
        <Composer
          store={drafts}
          target={target}
          draftKey={JSON.stringify(["feedback", ...address])}
          label="Feedback for the review agent"
          placeholder="What should the agent check, explain, or change?"
          draftLabel={`Feedback · ${reviewTitle}`}
          targetLabel="To the registered lead with this report and revision."
          targetTitle={`Report ${report.sha256.slice(0, 12)}`}
          disabled={Boolean(quote?.text.trim() && !quote.reference)}
          send={(text, submissionId) =>
            reportFeedback(target, {
              text,
              submissionId,
              quote: quote?.reference ? quote.text : "",
              quoteRef: quote?.reference ?? null,
              receiptId: report.receiptId,
              reportHash: report.sha256,
            })
          }
          onAccepted={() => {
            if (
              drafts.getSnapshot().entries.find((draft) => draft.key === quoteKey)?.text ===
              quoteDraft
            ) {
              writeQuote();
              setFeedbackNotice(undefined);
            }
          }}
        />
      </section>
      {report.semanticIndex === "ready" && (
        <details
          className="report-structured"
          onToggle={(event) => setContextOpen(event.currentTarget.open)}
        >
          <summary>Finding details and dispositions</summary>
          {contextOpen && (
            <section className="stack" aria-label="Structured review context">
              <div className="actions report-context">
                {(["findings", "changes", "sections"] as const).map((kind) => (
                  <button
                    type="button"
                    key={kind}
                    aria-pressed={kind === context}
                    onClick={() => setContext(kind)}
                  >
                    {kind === "findings"
                      ? "Finding dispositions"
                      : kind === "changes"
                        ? "Finding outcomes"
                        : "Source sections"}
                  </button>
                ))}
              </div>
              <ReportDetails
                key={context}
                run={run}
                report={report}
                collection={context}
                current={current}
                drafts={drafts}
                reviewTitle={reviewTitle}
                onSource={openSource}
              />
            </section>
          )}
        </details>
      )}
    </>
  );
}

function scrollWithinDrawer(element: HTMLElement) {
  const scroller = element.closest<HTMLElement>(".drawer-body");
  if (scroller)
    scroller.scrollTop +=
      element.getBoundingClientRect().top - scroller.getBoundingClientRect().top;
}

function readQuote(value: string): Quote | undefined {
  if (!value) return;
  try {
    const candidate: unknown = JSON.parse(value);
    if (
      !candidate ||
      typeof candidate !== "object" ||
      !("text" in candidate) ||
      !("reference" in candidate) ||
      typeof candidate.text !== "string"
    )
      return;
    const reference = candidate.reference;
    if (reference === null) return { text: candidate.text, reference };
    if (
      reference &&
      typeof reference === "object" &&
      "offset" in reference &&
      typeof reference.offset === "number" &&
      Number.isSafeInteger(reference.offset) &&
      reference.offset >= 0 &&
      "length" in reference &&
      reference.length === [...candidate.text].length &&
      "unit" in reference &&
      reference.unit === "unicode_codepoints"
    ) {
      return {
        text: candidate.text,
        reference: { offset: reference.offset, length: reference.length, unit: reference.unit },
      };
    }
  } catch {
    // A discarded or malformed memory-only draft has no valid quote.
  }
}
