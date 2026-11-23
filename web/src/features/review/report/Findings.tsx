import { useCallback, useEffect, useRef, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import type { ReportSummary, SourceOrigin } from "../../../api/generated/contracts/reports";
import { markReportViewed, reportView } from "../../../api/reports";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { describeError } from "../../../lib/presentation";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { FollowupResults } from "./FollowupResults";
import ReportDocument from "./ReportDocument";
import { ReportHistory } from "./ReportHistory";
import { Icon } from "../../../components/Icon";
import "./report.css";

type Props = {
  readonly run: RunDetail;
  readonly drafts: DraftStore;
  readonly reviewTitle: string;
  readonly webUrl: string;
  readonly followup: ResultSummary | null | undefined;
  readonly linkedCheck: ResultSummary | undefined;
  readonly onLinkHandled: () => void;
  readonly onOpenCheckRun: (check: ResultSummary) => void;
};

export default function Findings({
  run,
  drafts,
  reviewTitle,
  webUrl,
  followup,
  linkedCheck,
  onLinkHandled,
  onOpenCheckRun,
}: Props) {
  const bookmark = `report.receipt.${run.runId}`;
  const [receipt, setReceipt] = useState<string | undefined>(() => {
    if (linkedCheck) return linkedCheck.receiptId ?? undefined;
    const value = savedValue(bookmark);
    return typeof value === "string" && value.length <= 256 ? value : undefined;
  });
  const [historyLatest, setHistoryLatest] = useState<string>();
  const [sourceJump, setSourceJump] = useState<{
    report: SourceOrigin["report"];
    offset: number;
  }>();
  const [first, setFirst] = useState<ReportSummary>();
  const [frozen, setFrozen] = useState<ReportSummary>();
  const [history, setHistory] = useState(false);
  const [checksEpoch, setChecksEpoch] = useState(0);
  const [dismissedCheck, setDismissedCheck] = useState<string>();
  const [check, setCheck] = useState<ResultSummary | undefined>(linkedCheck);
  useEffect(() => {
    if (linkedCheck) {
      saveValue(bookmark, linkedCheck.receiptId);
      onLinkHandled();
    }
  }, [linkedCheck, bookmark, onLinkHandled]);
  const [notice, setNotice] = useState<string>();
  const [busy, setBusy] = useState(false);

  const historyTrigger = useRef<HTMLButtonElement>(null);
  const viewed = useRef(
    new Map<
      string,
      { submissionId: string; pending: boolean; done: boolean; automatic: boolean }
    >(),
  );
  const current = useResource(
    `latest-report:${run.runId}`,
    async (signal) => {
      const result = await reportView(run.runId, signal);
      const latest = result.data.report;
      if (latest && !signal.aborted) setFirst((value) => value ?? latest);
      return result;
    },
    [{ kind: "report", id: run.runId }],
  );
  const chosen = receipt ?? first?.receiptId;
  const loadedLatest = current.data?.report?.receiptId;
  const latestId = current.data ? loadedLatest : historyLatest;
  const historical = useResource(
    `report-version:${run.runId}:${chosen ?? ""}:${loadedLatest ?? ""}`,
    (signal) =>
      chosen && chosen !== loadedLatest
        ? reportView(run.runId, signal, chosen)
        : Promise.resolve({ data: null, cursor: "" }),
    [{ kind: "report", id: run.runId }],
  );
  const report =
    chosen === loadedLatest
      ? current.data?.report
      : (historical.data?.report ??
        (frozen?.receiptId === chosen ? frozen : undefined) ??
        (first?.receiptId === chosen ? first : undefined));
  const delivery = current.data?.delivery;
  const displayedReceipt = report?.receiptId;
  const displayedCheck =
    check ??
    (followup?.kind === "unchanged" &&
    followup.runId === run.runId &&
    displayedReceipt &&
    followup.receiptId === displayedReceipt &&
    followup.batchId !== dismissedCheck
      ? followup
      : undefined);
  const isHistorical = Boolean(chosen && latestId && chosen !== latestId && receipt);
  const outdated = Boolean(chosen && latestId && chosen !== latestId && !receipt);
  const returnFocus = useRef(false);
  const bindHistory = useCallback((button: HTMLButtonElement | null) => {
    if (!button) return;
    historyTrigger.current = button;
    if (returnFocus.current && document.activeElement === document.body)
      button.focus({ preventScroll: true });
    returnFocus.current = false;
    return () => {
      returnFocus.current = returnFocus.current || document.activeElement === button;
      historyTrigger.current = null;
    };
  }, []);

  function choose(
    id: string | undefined,
    closeCheck = true,
    summary?: ReportSummary,
    source?: typeof sourceJump,
  ) {
    returnFocus.current =
      history ||
      document.activeElement === historyTrigger.current ||
      Boolean(document.activeElement?.closest(".report-history-access"));
    setFrozen(
      summary ?? (id && current.data?.report?.receiptId === id ? current.data.report : undefined),
    );
    setReceipt(id);
    setSourceJump(source);
    setNotice(undefined);
    setHistory(false);
    if (closeCheck) {
      setCheck(undefined);
      setDismissedCheck(undefined);
      setChecksEpoch((value) => value + 1);
    }
    saveValue(bookmark, id ?? null);
  }

  async function acknowledge(receiptId: string, renderedBatchId?: string, automatic = false) {
    if (receiptId !== displayedReceipt) return false;
    const key = JSON.stringify([receiptId, renderedBatchId]);
    const known = viewed.current.get(key);
    if (known?.done) return true;
    if (known?.pending || (automatic && known?.automatic)) return false;
    const attempt = known ?? {
      submissionId: crypto.randomUUID(),
      pending: false,
      done: false,
      automatic: false,
    };
    viewed.current.set(key, attempt);
    attempt.pending = true;
    attempt.automatic ||= automatic;
    if (!automatic) {
      setBusy(true);
      setNotice(undefined);
    }
    try {
      await markReportViewed(run.runId, {
        receiptId,
        submissionId: attempt.submissionId,
        ...(renderedBatchId ? { renderedBatchId } : {}),
      });
      attempt.done = true;
      if (!automatic)
        setNotice(
          renderedBatchId
            ? "Follow-up result marked viewed."
            : "This findings version was marked viewed.",
        );
      return true;
    } catch (error) {
      if (!automatic) setNotice(describeError(error));
      return false;
    } finally {
      attempt.pending = false;
      if (!automatic) setBusy(false);
    }
  }

  const historyAccess = (
    <span className="report-history-access">
      {isHistorical && (
        <>
          <span className="report-version-label">Earlier version</span>
          <button
            type="button"
            className="quiet-button"
            title="Viewing an earlier version"
            onClick={() => choose(latestId)}
          >
            Back to latest
          </button>
        </>
      )}
      <button
        type="button"
        ref={bindHistory}
        className="quiet-button history-trigger"
        aria-haspopup="dialog"
        aria-expanded={history}
        onClick={() => setHistory(true)}
      >
        <Icon name="history" size={14} />
        History
      </button>
    </span>
  );

  return (
    <div className="findings stack">
      {delivery && delivery.state !== "saved" && (
        <Notice>
          {delivery.state === "failed" ? "Report handoff failed" : "Report completion pending"}
          {delivery.reason && ` · ${reportHandoffReason(delivery.reason)}`}
        </Notice>
      )}
      {outdated && (
        <Notice>
          Updated findings available.{" "}
          <button type="button" onClick={() => choose(latestId)}>
            Open latest findings
          </button>
        </Notice>
      )}
      {current.error && <Notice retry={current.retry}>{current.error}</Notice>}
      {historical.error && chosen !== loadedLatest && (
        <Notice retry={historical.retry}>
          {historical.error}
          {latestId && (
            <button type="button" onClick={() => choose(latestId)}>
              Open latest findings
            </button>
          )}
        </Notice>
      )}
      {notice && <Notice>{notice}</Notice>}
      {history && (
        <ReportHistory
          run={run}
          report={report ?? undefined}
          latestId={latestId}
          selected={chosen}
          selectedBatch={displayedCheck?.batchId}
          onClose={() => setHistory(false)}
          onVersion={(id, summary, latest) => {
            setHistoryLatest(latest);
            choose(id, true, summary);
          }}
          onCheck={(batch) => {
            setCheck(batch);
            setHistory(false);
            if (
              batch.runId === run.runId &&
              batch.receiptId &&
              batch.receiptId !== displayedReceipt
            )
              choose(batch.receiptId, false);
          }}
        />
      )}
      <FollowupResults
        key={checksEpoch}
        run={run}
        batch={displayedCheck}
        onDismiss={() => {
          setDismissedCheck(displayedCheck?.batchId);
          setCheck(undefined);
        }}
        receipt={displayedReceipt}
        disabled={busy}
        onOpenReceipt={(id) => choose(id, false)}
        onOpenRun={onOpenCheckRun}
        onAcknowledge={(batch, automatic) =>
          batch.runId === run.runId && batch.receiptId === displayedReceipt && displayedReceipt
            ? acknowledge(displayedReceipt, batch.batchId, automatic)
            : Promise.resolve(false)
        }
      />
      {!report && (chosen || current.data || current.error) && <div>{historyAccess}</div>}
      {!report &&
        (current.pending || (chosen && chosen !== loadedLatest && historical.pending) ? (
          <Loading label="Loading accepted findings…" />
        ) : (
          current.data && !chosen && <Empty title="No accepted findings yet" />
        ))}
      {report && (
        <article className="stack" aria-label="Accepted findings">
          <ReportDocument
            key={JSON.stringify([report.receiptId, report.sha256])}
            run={run}
            report={report}
            current={chosen === latestId}
            historyAccess={historyAccess}
            drafts={drafts}
            reviewTitle={reviewTitle}
            webUrl={webUrl}
            initialSourceOffset={
              sourceJump?.report.receiptId === report.receiptId &&
              sourceJump.report.sha256 === report.sha256
                ? sourceJump.offset
                : undefined
            }
            onOpenEvidence={(origin, offset) =>
              choose(origin.report.receiptId, true, undefined, { report: origin.report, offset })
            }
            onRendered={(id) => void acknowledge(id, undefined, true)}
            onMarkViewed={() => void acknowledge(report.receiptId)}
            markingViewed={busy}
          />
        </article>
      )}
    </div>
  );
}

function reportHandoffReason(reason: string) {
  switch (reason) {
    case "report_base_changed":
      return "ReviewQ could not confirm this handoff still applies to the report. Check the current Findings before submitting again.";
    case "report_amendment_continuity_unproved":
      return "An amendment keeps the accepted Markdown, reviewed revision, and verdict. Use an evidence correction for changes that preserve the revision and verdict.";
    case "report_unchanged_evidence_mismatch":
      return "An unchanged result uses the current accepted receipt and its original source.";
    case "report_presentation_evidence_unproved":
      return "Submit an evidence correction for claims the cited section does not establish.";
    default:
      return reason;
  }
}
