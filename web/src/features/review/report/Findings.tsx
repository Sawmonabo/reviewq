import { lazy, Suspense, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { markReportViewed, reportDownload, reportHistory, reportView } from "../../../api/reports";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { describeError, formatActivityTime, formatTime, readable } from "../../../lib/presentation";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { FollowupResults } from "./FollowupResults";
import "./report.css";

const ReportDocument = lazy(() => import("./ReportDocument"));

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
  const [cursor, setCursor] = useState<string>();
  const [notice, setNotice] = useState<string>();
  const [busy, setBusy] = useState(false);
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

  function choose(id: string | undefined) {
    const shown =
      current.data?.report?.receiptId === id
        ? current.data?.report
        : versions.data?.items.find((version) => version.receiptId === id);
    setFrozen(shown ?? undefined);
    setReceipt(id);
    setNotice(undefined);
    saveValue(bookmark, id ?? null);
  }
  async function acknowledge(renderedBatchId?: string) {
    if (!report || busy) return false;
    setBusy(true);
    setNotice(undefined);
    try {
      await markReportViewed(run.runId, {
        receiptId: report.receiptId,
        submissionId: crypto.randomUUID(),
        ...(renderedBatchId ? { renderedBatchId } : {}),
      });
      setNotice(
        renderedBatchId
          ? "Follow-up result marked viewed."
          : "This findings version was marked viewed.",
      );
      return true;
    } catch (error) {
      setNotice(describeError(error));
      return false;
    } finally {
      setBusy(false);
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
      {chosen && latestId && chosen !== latestId && (
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
        <FollowupResults
          run={run}
          latest={followup}
          receipt={report?.receiptId}
          disabled={busy}
          onOpenReceipt={choose}
          onOpenRun={onRun}
          onAcknowledge={(batch) =>
            batch.runId === run.runId && batch.receiptId === report?.receiptId
              ? acknowledge(batch.batchId)
              : Promise.resolve(false)
          }
        />
        <button
          type="button"
          className="quiet-button"
          aria-expanded={history}
          onClick={() => setHistory(!history)}
        >
          Report history
        </button>
      </div>
      {history && (
        <section className="report-history-list stack" aria-label="Report versions">
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
              onClick={() => choose(version.receiptId)}
            >
              <time dateTime={version.capturedAt} title={formatTime(version.capturedAt)}>
                {formatActivityTime(version.capturedAt)}
              </time>
              <span className="meta" title={version.sha256}>
                {version.receiptId === latestId ? "Latest" : readable(version.verdict)}
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
      {!report &&
        (current.pending || (chosen && chosen !== latestId && historical.pending) ? (
          <Loading label="Loading accepted findings…" />
        ) : (
          current.data && !chosen && <Empty title="No accepted findings yet" />
        ))}
      {report && (
        <article className="stack" aria-label="Accepted findings">
          <Suspense fallback={<Loading label="Loading report reader…" />}>
            <ReportDocument
              key={report.receiptId}
              run={run}
              report={report}
              current={chosen === latestId}
              drafts={drafts}
              reviewTitle={reviewTitle}
              webUrl={webUrl}
            />
          </Suspense>
          <details className="report-version-meta">
            <summary>Report version details</summary>
            <div className="stack">
              <p className="meta">
                {chosen === latestId ? "Current" : "Historical"} · {formatTime(report.capturedAt)} ·{" "}
                {readable(report.coverage)}
              </p>
              <p className="meta">
                Reviewed revision ·{" "}
                <code title={report.reviewedSha ?? undefined}>
                  {report.reviewedSha?.slice(0, 12) ?? "Unknown"}
                </code>
              </p>
              {report.revisionReason && <p>{report.revisionReason}</p>}
              <div className="actions">
                <a href={reportDownload(run.runId, report.receiptId)}>Download Markdown</a>
                <button type="button" disabled={busy} onClick={() => void acknowledge()}>
                  Mark findings viewed
                </button>
              </div>
            </div>
          </details>
        </article>
      )}
    </div>
  );
}
