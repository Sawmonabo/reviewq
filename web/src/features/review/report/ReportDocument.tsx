import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { QuoteRef, ReportSummary } from "../../../api/generated/contracts/reports";
import {
  reportContext,
  reportDownload,
  reportFeedback,
  resolveReportQuote,
  reportView,
  reportWindow,
} from "../../../api/reports";
import { ActionMenu } from "../../../components/ActionMenu";
import { Icon } from "../../../components/Icon";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { parseSettledMarkdown } from "../../../components/markdown/settled";
import { useDraft } from "../../../hooks/useDraft";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { formatTime, readable, safeUrl } from "../../../lib/presentation";
import { Composer } from "../Composer";
import { ReportDetails } from "./ReportDetails";
import { ReportSource } from "./ReportSource";
import { ReportTable } from "./ReportTable";
import { RichReport } from "./RichReport";
import { finishReading, readBookmark } from "./report-position";
import { canWindowReport, wholeDocument, indexedDocument, reportMarkdown } from "./report-reader";
import type { AcceptedFindingChange } from "./report-structure";
import { useVisibleContent } from "./useVisibleContent";

type Quote = { readonly text: string; readonly reference: QuoteRef | null };
type Props = {
  readonly historyAccess: ReactNode;
  readonly run: RunDetail;
  readonly report: ReportSummary;
  readonly current: boolean;
  readonly drafts: DraftStore;
  readonly reviewTitle: string;
  readonly webUrl: string;
  readonly onRendered: (receiptId: string) => void;
  readonly onMarkViewed: () => void;
  readonly markingViewed: boolean;
};

export default function ReportDocument({
  historyAccess,
  run,
  report,
  current,
  drafts,
  reviewTitle,
  webUrl,
  onRendered,
  onMarkViewed,
  markingViewed,
}: Props) {
  const [source, setSource] = useState(false);
  const [sourceOffset, setSourceOffset] = useState(0);
  const [tableSource, setTableSource] = useState<{ sha256: string; byte: number }>();
  const [notice, setNotice] = useState<string>();
  const [feedbackNotice, setFeedbackNotice] = useState<string>();
  const [context, setContext] = useState<"findings" | "changes" | "sections">();
  const contextTitle =
    context === "findings"
      ? "Finding dispositions"
      : context === "changes"
        ? "Finding outcomes"
        : "Source sections";
  const feedback = useRef<HTMLElement>(null);
  const reader = useRef<HTMLDivElement>(null);
  const reportActions = useRef<HTMLButtonElement>(null);
  const address = [run.reviewId, run.runId, run.leadSessionKey, report.receiptId, report.sha256];
  const quoteKey = JSON.stringify(["quote", ...address]);
  const quoteLabel = `Quote · ${reviewTitle}`;
  const quoteDraft = useDraft(drafts, quoteKey);
  const quote = readQuote(quoteDraft);
  const admitted =
    report.byteLength <= 1024 * 1024 && report.lineCount <= 10_000 && report.maxLineBytes <= 16_384;
  const bookmark = `findings.position.${run.runId}.${report.receiptId}.${report.sha256}`;
  // Opened thread tables by report offset, kept while the reader is replaced or reads source,
  // and saved with the reading position.
  const [openFolds, setOpenFolds] = useState<ReadonlySet<number>>(
    () => new Set(readBookmark(bookmark)?.folds),
  );
  const toggleFold = useCallback(
    (table: number) =>
      setOpenFolds((previous) => {
        const next = new Set(previous);
        if (!next.delete(table)) next.add(table);
        return next;
      }),
    [],
  );
  const full = useRef(false);
  const body = useResource(
    JSON.stringify(["accepted-document", run.runId, report.receiptId, report.sha256]),
    async (signal) => {
      if (!admitted) return { cursor: "", data: null };
      const position = readBookmark(bookmark);
      const result = await loadReport(
        run.runId,
        report,
        safeUrl(webUrl),
        position,
        full.current,
        signal,
      );
      // The immutable receipt can refresh its accepted outcomes or formatting.
      // A selection made while that response was in flight still belongs to the live DOM.
      await finishReading(reader.current?.querySelector(".report-blocks") ?? null, signal);
      signal.throwIfAborted();
      return result;
    },
  );
  const displayed = body.data;
  const resolving = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => resolving.current?.abort(), [quoteKey]);
  const formattingFailure = Boolean(displayed?.formattingFailure);
  const sourceMode = source || !admitted || formattingFailure || Boolean(body.error);
  function fallback() {
    if (!full.current) {
      full.current = true;
      body.retry();
    }
  }
  const target = { runId: run.runId, sessionKey: run.leadSessionKey };
  useVisibleContent(
    reader,
    !sourceMode && displayed?.reader ? report.receiptId : undefined,
    onRendered,
    ".report-authored",
  );

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
    resolving.current?.abort();
    resolving.current = undefined;
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

  async function verifyPassage() {
    const text = quote?.text;
    if (!text?.trim() || quote?.reference) return;
    if ([...text].length > 12_000) {
      setFeedbackNotice("Select a non-empty quote up to 12,000 characters.");
      return;
    }
    resolving.current?.abort();
    const controller = new AbortController();
    resolving.current = controller;
    function currentDraft() {
      return (
        !controller.signal.aborted &&
        drafts.getSnapshot().entries.find((entry) => entry.key === quoteKey)?.text === quoteDraft
      );
    }
    try {
      const result = await resolveReportQuote(run.runId, report, text, controller.signal);
      if (!currentDraft()) return;
      if (result.state === "unique") capture(text, result.offset, false);
      else
        setFeedbackNotice(
          result.state === "ambiguous"
            ? "This passage appears more than once. Select the intended occurrence in the report."
            : "Select an exact passage in the report or original source.",
        );
    } catch {
      if (currentDraft()) setFeedbackNotice("Could not check this passage. Try again.");
    } finally {
      if (resolving.current === controller) resolving.current = undefined;
    }
  }

  const tryTable =
    !source &&
    (!admitted || formattingFailure) &&
    report.byteLength >= 8 * 1024 &&
    report.byteLength <= 32 * 1024 * 1024;
  function openSource(offset = 0) {
    setSourceOffset(offset);
    setSource(true);
    const element = reader.current;
    if (element)
      requestAnimationFrame(() => {
        if (element.isConnected) scrollWithinDrawer(element);
      });
  }

  const sourceView = (
    <ReportSource
      historyAccess={historyAccess}
      runId={run.runId}
      report={report}
      requestedOffset={sourceOffset}
      reason={
        !admitted
          ? "Large report · source view"
          : formattingFailure
            ? "Report formatting limit · Original source"
            : body.error
              ? "Formatted report unavailable · Original source"
              : "Original Markdown"
      }
      onQuote={capture}
      onRendered={onRendered}
    />
  );
  return (
    <>
      <div className="report-reader" ref={reader}>
        {!sourceMode && (body.pending || !displayed?.reader) && historyAccess}
        {notice && <Notice>{notice}</Notice>}
        {!sourceMode && displayed?.outcomeError && (
          <Notice retry={body.retry}>Accepted finding outcomes are unavailable.</Notice>
        )}
        {sourceMode ? (
          tryTable ? (
            <ReportTable
              historyAccess={historyAccess}
              runId={run.runId}
              report={report}
              base={safeUrl(webUrl)}
              opening={tableSource?.sha256 === report.sha256 ? tableSource.byte : 0}
              onSource={(offset, byte) => {
                setTableSource({ sha256: report.sha256, byte });
                openSource(offset);
              }}
              onQuote={capture}
              onRendered={onRendered}
              fallback={sourceView}
            />
          ) : (
            sourceView
          )
        ) : body.pending ? (
          <Loading label="Loading accepted report…" />
        ) : displayed?.reader ? (
          <RichReport
            historyAccess={historyAccess}
            document={displayed.reader}
            report={report}
            onFallback={fallback}
            bookmark={bookmark}
            onQuote={capture}
            onSource={(offset) => {
              openSource(offset);
              setNotice("Select the exact passage in the original Markdown to attach a quote.");
            }}
            openFolds={openFolds}
            onFold={toggleFold}
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
        {(admitted || (source && tableSource?.sha256 === report.sha256)) && (
          <button
            type="button"
            className="quiet-button"
            onClick={() => {
              setNotice(undefined);
              if (source) setSource(false);
              else openSource();
            }}
          >
            {source
              ? admitted
                ? "Read formatted report"
                : "Return to table rows"
              : "Read original source"}
          </button>
        )}
        <ActionMenu
          label="Report actions"
          trigger={<Icon name="more" />}
          triggerRef={reportActions}
          items={[
            ...(report.findingCount !== 0
              ? [{ label: "Finding dispositions", run: () => setContext("findings") }]
              : []),
            ...(report.changeCount !== 0
              ? [{ label: "Finding outcomes", run: () => setContext("changes") }]
              : []),
            { label: "Source sections", run: () => setContext("sections") },
            { label: "Download Markdown", href: reportDownload(run.runId, report.receiptId) },
            { label: "Mark findings viewed", run: onMarkViewed, disabled: markingViewed },
          ]}
        />
      </div>
      {context && (
        <section className="report-structured stack" aria-label={contextTitle}>
          <div className="spread">
            <h3>{contextTitle}</h3>
            <button
              className="quiet-button"
              type="button"
              onClick={() => {
                setContext(undefined);
                reportActions.current?.focus({ preventScroll: true });
              }}
            >
              Close
            </button>
          </div>
          <ReportDetails
            key={context}
            run={run}
            report={report}
            collection={context}
            current={current}
            drafts={drafts}
            reviewTitle={reviewTitle}
            onSource={(range) => openSource(range.offset)}
          />
        </section>
      )}
      <span className="sr-only">
        {current ? "Current" : "Historical"} · {formatTime(report.publishedAt)} ·{" "}
        {readable(report.coverage)}
        {report.reviewedSha && ` · Reviewed revision ${report.reviewedSha}`}
        {` · ${report.findingCount} indexed findings`}
        {` · ${report.changeCount} declared changes`}
        {` · SHA-256 ${report.sha256}`}
        {report.revisionReason && ` · ${report.revisionReason}`}
      </span>
      <section className="report-feedback stack" aria-label="Report feedback" ref={feedback}>
        <label className="field report-quote-field">
          Finding or passage to reference
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
                onClick={() => void verifyPassage()}
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
    </>
  );
}

async function loadReport(
  runId: string,
  report: ReportSummary,
  base: string | undefined,
  position: ReturnType<typeof readBookmark>,
  full: boolean,
  signal: AbortSignal,
) {
  const eligible = !full && canWindowReport(report);
  const [outcomes, entry] = await Promise.all([
    report.changeCount
      ? acceptedChanges(runId, report, signal)
          .then((changes) => ({ changes, error: false }))
          .catch(() => {
            signal.throwIfAborted();
            return { changes: undefined, error: true };
          })
      : Promise.resolve({ changes: undefined, error: false }),
    eligible
      ? reportWindow(runId, report, signal, {
          layout: true,
          ...(position?.byte !== undefined && { byteOffset: position.byte }),
        })
          .then((result) => ({ window: result.data.result, cursor: result.cursor }))
          .catch(() => {
            signal.throwIfAborted();
            return undefined;
          })
      : reportView(runId, signal, report.receiptId, "markdown").then((result) => ({
          body: result.data,
          cursor: result.cursor,
        })),
  ]);
  signal.throwIfAborted();
  if (entry && "window" in entry && entry.window.state === "ready" && entry.window.layout) {
    let indexed;
    try {
      indexed = indexedDocument(
        runId,
        report,
        entry.window,
        outcomes.changes,
        base,
        position?.offset,
      );
    } catch {
      signal.throwIfAborted();
    }
    if (indexed) {
      return {
        cursor: entry.cursor,
        data: {
          reader: indexed,
          formattingFailure: false,
          outcomeError: outcomes.error,
        },
      };
    }
  }
  const response =
    entry && "body" in entry
      ? { cursor: entry.cursor, data: entry.body }
      : await reportView(runId, signal, report.receiptId, "markdown");
  signal.throwIfAborted();
  const markdown =
    response.data.report?.sha256 === report.sha256 ? response.data.markdown : undefined;
  let tree;
  try {
    if (typeof markdown === "string")
      tree = parseSettledMarkdown(
        markdown,
        [{ markdown, table: response.data.display?.table }],
        base,
        reportMarkdown,
      );
  } catch {
    tree = undefined;
  }
  return {
    cursor: response.cursor,
    data: {
      reader:
        tree && typeof markdown === "string"
          ? wholeDocument(markdown, tree, outcomes.changes, response.data.display)
          : undefined,
      formattingFailure: typeof markdown === "string" && !tree,
      outcomeError: outcomes.error,
    },
  };
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

// Reads every outcome recorded on the report. The count must match the report's own count.
async function acceptedChanges(runId: string, report: ReportSummary, signal: AbortSignal) {
  const changes = new Map<string, AcceptedFindingChange>();
  let next: string | undefined;
  do {
    // The server halves a page until it fits the 64 KiB read; this only sets the usual size.
    const result = await reportContext(runId, report.receiptId, "changes", next, signal, 20);
    if (
      result.data.baseReport?.receiptId !== report.receiptId ||
      result.data.baseReport.sha256 !== report.sha256
    )
      throw new Error("The report changed while its outcomes were loading.");
    for (const item of result.data.items) {
      if (item.kind !== "change" || changes.has(item.findingId))
        throw new Error("The report's outcomes repeat a finding or include another kind of item.");
      changes.set(item.findingId, item);
    }
    const continuation = result.data.nextCursor;
    if (
      continuation &&
      (!result.data.items.length || continuation === next || changes.size >= report.changeCount)
    )
      throw new Error("The report's outcomes ran past their recorded count or stopped advancing.");
    next = continuation ?? undefined;
  } while (next);
  if (changes.size !== report.changeCount)
    throw new Error("The report has fewer outcomes than it records.");
  return changes;
}
