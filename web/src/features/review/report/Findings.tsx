import { useEffect, useRef, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { markReportViewed, reportHistory, reportView } from "../../../api/reports";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { describeError, formatActivityTime, formatTime, readable } from "../../../lib/presentation";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { FollowupResults } from "./FollowupResults";
import ReportDocument from "./ReportDocument";
import "./report.css";

type Props = {
  readonly run: RunDetail;
  readonly drafts: DraftStore;
  readonly reviewTitle: string;
  readonly webUrl: string;
  readonly followup: ResultSummary | null | undefined;
  readonly onRun: (run: string) => void;
};

export default function Findings({ run, drafts, reviewTitle, webUrl, followup, onRun }: Props) {
  const bookmark = `report.receipt.${run.runId}`;
  const [receipt, setReceipt] = useState<string | undefined>(() => {
    const value = savedValue(bookmark);
    return typeof value === "string" && value.length <= 256 ? value : undefined;
  });
  const [first, setFirst] = useState<ReportSummary>();
  const [frozen, setFrozen] = useState<ReportSummary>();
  const [history, setHistory] = useState(false);
  const [checksEpoch, setChecksEpoch] = useState(0);
  const [cursor, setCursor] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [locatingEvidence, setLocatingEvidence] = useState(false);
  const evidenceRequest = useRef<AbortController | undefined>(undefined);
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
  const latestId = current.data?.report?.receiptId;
  const historical = useResource(
    `report-version:${run.runId}:${chosen ?? ""}:${latestId ?? ""}`,
    (signal) =>
      chosen && chosen !== latestId
        ? reportView(run.runId, signal, chosen)
        : Promise.resolve({ data: null, cursor: "" }),
    [{ kind: "report", id: run.runId }],
  );
  const versions = useResource(
    `report-history:${run.runId}:${history}:${cursor ?? ""}`,
    (signal) =>
      history
        ? reportHistory(run.runId, cursor, signal)
        : Promise.resolve({ data: null, cursor: "" }),
    [{ kind: "report", id: run.runId }],
  );
  const report =
    chosen === latestId
      ? current.data?.report
      : (historical.data?.report ??
        (frozen?.receiptId === chosen ? frozen : undefined) ??
        (first?.receiptId === chosen ? first : undefined));
  const delivery = current.data?.delivery;
  const displayedReceipt = report?.receiptId;
  const isHistorical = Boolean(chosen && latestId && chosen !== latestId && receipt);
  const outdated = Boolean(chosen && latestId && chosen !== latestId && !receipt);
  useEffect(
    () => () => {
      evidenceRequest.current?.abort();
      evidenceRequest.current = undefined;
    },
    [run.runId],
  );

  function choose(id: string | undefined, closeCheck = true) {
    const shown =
      current.data?.report?.receiptId === id
        ? current.data?.report
        : versions.data?.items.find((version) => version.receiptId === id);
    setFrozen(shown ?? undefined);
    setReceipt(id);
    setNotice(undefined);
    setHistory(false);
    if (closeCheck) setChecksEpoch((value) => value + 1);
    evidenceRequest.current?.abort();
    evidenceRequest.current = undefined;
    setLocatingEvidence(false);
    saveValue(bookmark, id ?? null);
  }

  async function showEarlierEvidence() {
    if (!report?.parentReceiptId || evidenceRequest.current) return;
    const controller = new AbortController();
    evidenceRequest.current = controller;
    setLocatingEvidence(true);
    setNotice(undefined);
    try {
      const earlier = await earlierEvidence(run.runId, report, controller.signal);
      if (!controller.signal.aborted) choose(earlier);
    } catch (error) {
      if (!controller.signal.aborted) setNotice(describeError(error));
    } finally {
      if (evidenceRequest.current === controller) {
        evidenceRequest.current = undefined;
        setLocatingEvidence(false);
      }
    }
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

  return (
    <div className="findings stack">
      {delivery && delivery.state !== "saved" && (
        <Notice>
          {delivery.state === "failed" ? "Report handoff failed" : "Report completion pending"}
          {delivery.reason && ` · ${delivery.reason}`}
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
      {historical.error && chosen !== latestId && (
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
      <div className="report-tools">
        {isHistorical && (
          <button type="button" className="quiet-button" onClick={() => choose(latestId)}>
            Back to current findings
          </button>
        )}
        {!isHistorical && report?.parentReceiptId && (
          <button
            type="button"
            className="quiet-button"
            disabled={locatingEvidence}
            onClick={() => void showEarlierEvidence()}
          >
            {locatingEvidence ? "Opening earlier evidence…" : "Earlier evidence"}
          </button>
        )}
        <button
          ref={historyTrigger}
          type="button"
          className="quiet-button"
          aria-expanded={history}
          onClick={() => setHistory(!history)}
        >
          Report history
        </button>
      </div>
      {history && (
        <section className="report-history-list stack" aria-label="Report history">
          {versions.pending && <Loading label="Reading accepted report versions…" />}
          {versions.error && <Notice retry={versions.retry}>{versions.error}</Notice>}
          {!versions.data?.items.length && !versions.pending && (
            <p className="meta">No accepted versions in this page.</p>
          )}
          {versions.data?.items.map((version) => (
            <button
              type="button"
              className="report-version quiet-button"
              key={version.receiptId}
              aria-pressed={chosen === version.receiptId}
              onClick={() => {
                historyTrigger.current?.focus({ preventScroll: true });
                choose(version.receiptId);
              }}
            >
              <time dateTime={version.capturedAt} title={formatTime(version.capturedAt)}>
                {formatActivityTime(version.capturedAt)}
              </time>
              <span className="meta" title={`${readable(version.verdict)} · ${version.sha256}`}>
                {version.receiptId === latestId ? "Latest" : "Earlier findings"}
              </span>
            </button>
          ))}
          <div className="actions">
            {cursor && (
              <button type="button" onClick={() => setCursor(undefined)}>
                Latest versions
              </button>
            )}
            {versions.data?.nextCursor && (
              <button
                type="button"
                onClick={() => setCursor(versions.data?.nextCursor ?? undefined)}
              >
                Older versions
              </button>
            )}
          </div>
        </section>
      )}
      <FollowupResults
        key={checksEpoch}
        run={run}
        latest={followup}
        open={history}
        receipt={displayedReceipt}
        disabled={busy}
        onOpenReceipt={(id) => choose(id, false)}
        onOpenRun={onRun}
        onChoose={(batch) => {
          historyTrigger.current?.focus({ preventScroll: true });
          setHistory(false);
          if (batch.runId === run.runId && batch.receiptId && batch.receiptId !== displayedReceipt)
            choose(batch.receiptId, false);
        }}
        onAcknowledge={(batch, automatic) =>
          batch.runId === run.runId && batch.receiptId === displayedReceipt && displayedReceipt
            ? acknowledge(displayedReceipt, batch.batchId, automatic)
            : Promise.resolve(false)
        }
      />
      {!report &&
        (current.pending || (chosen && chosen !== latestId && historical.pending) ? (
          <Loading label="Loading accepted findings…" />
        ) : (
          current.data && !chosen && <Empty title="No accepted findings yet" />
        ))}
      {report && (
        <article className="stack" aria-label="Accepted findings">
          <ReportDocument
            key={report.receiptId}
            run={run}
            report={report}
            current={chosen === latestId}
            drafts={drafts}
            reviewTitle={reviewTitle}
            webUrl={webUrl}
            onRendered={(id) => void acknowledge(id, undefined, true)}
            onMarkViewed={() => void acknowledge(report.receiptId)}
            markingViewed={busy}
          />
        </article>
      )}
    </div>
  );
}

async function earlierEvidence(runId: string, report: ReportSummary, signal: AbortSignal) {
  let parent = report.parentReceiptId;
  if (report.coverage !== "selected_threads" && parent) return parent;
  const seen = new Set([report.receiptId]);
  while (parent && seen.size <= 32 && !seen.has(parent)) {
    seen.add(parent);
    const response = await reportView(runId, signal, parent);
    const previous = response.data.report;
    if (!previous) break;
    if (previous.coverage !== "selected_threads" || !previous.parentReceiptId)
      return previous.receiptId;
    parent = previous.parentReceiptId;
  }
  throw new Error(
    "The earlier evidence could not be confirmed. Choose an accepted version from Report history.",
  );
}
