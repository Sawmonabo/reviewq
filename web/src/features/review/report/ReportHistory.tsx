import { useEffect, useRef, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { reportHistory, reportView } from "../../../api/reports";
import { Modal } from "../../../components/Modal";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { describeError, formatActivityTime, formatTime, readable } from "../../../lib/presentation";
import { ThreadCheckHistory } from "./ThreadCheckHistory";

export function ReportHistory({
  run,
  report,
  latestId,
  selected,
  selectedBatch,
  onVersion,
  onCheck,
  onClose,
}: {
  readonly run: RunDetail;
  readonly report: ReportSummary | undefined;
  readonly latestId: string | undefined;
  readonly selected: string | undefined;
  readonly selectedBatch: string | undefined;
  readonly onVersion: (
    id: string,
    summary: ReportSummary | undefined,
    latest: string | undefined,
  ) => void;
  readonly onCheck: (result: ResultSummary) => void;
  readonly onClose: () => void;
}) {
  const [cursor, setCursor] = useState<string>();
  const [error, setError] = useState<string>();
  const [locatingEvidence, setLocatingEvidence] = useState(false);
  const evidenceRequest = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => evidenceRequest.current?.abort(), []);
  const versions = useResource(
    `report-history:${run.runId}:${cursor ?? ""}`,
    (signal) => reportHistory(run.runId, cursor, signal),
    [{ kind: "report", id: run.runId }],
  );
  const currentReceipt = versions.data ? (versions.data.currentReceiptId ?? undefined) : latestId;
  async function showEarlierEvidence() {
    if (!report?.parentReceiptId || evidenceRequest.current) return;
    const controller = new AbortController();
    evidenceRequest.current = controller;
    setLocatingEvidence(true);
    setError(undefined);
    try {
      const earlier = await earlierEvidence(run.runId, report, controller.signal);
      if (!controller.signal.aborted) onVersion(earlier, undefined, currentReceipt);
    } catch (error) {
      if (!controller.signal.aborted) setError(describeError(error));
    } finally {
      if (evidenceRequest.current === controller) {
        evidenceRequest.current = undefined;
        setLocatingEvidence(false);
      }
    }
  }

  return (
    <div
      className="report-history-layer"
      onKeyDown={(event) => {
        if (event.key === "Escape") event.stopPropagation();
      }}
    >
      <Modal title="History" className="report-history-dialog" onClose={onClose}>
        {report?.parentReceiptId && report.receiptId === currentReceipt && (
          <button
            type="button"
            className="quiet-button earlier-evidence"
            disabled={locatingEvidence}
            onClick={() => void showEarlierEvidence()}
          >
            {locatingEvidence ? "Opening earlier evidence…" : "Earlier evidence"}
          </button>
        )}
        {error && <Notice>{error}</Notice>}
        <section className="report-history-list stack" aria-label="Report history">
          <h3 className="report-check-heading">Report versions</h3>
          {versions.pending && <Loading label="Reading accepted report versions…" />}
          {versions.error && <Notice retry={versions.retry}>{versions.error}</Notice>}
          {versions.data?.items.length === 0 && (
            <p className="meta">No accepted versions in this page.</p>
          )}
          {versions.data?.items.map((version) => (
            <button
              type="button"
              className="report-version quiet-button"
              key={version.receiptId}
              aria-pressed={selected === version.receiptId}
              onClick={() => onVersion(version.receiptId, version, currentReceipt)}
            >
              <time dateTime={version.capturedAt} title={formatTime(version.capturedAt)}>
                {formatActivityTime(version.capturedAt)}
              </time>
              <span className="meta" title={`${readable(version.verdict)} · ${version.sha256}`}>
                {version.receiptId === currentReceipt ? "Latest" : "Earlier findings"}
              </span>
            </button>
          ))}
          {(cursor || versions.data?.nextCursor) && (
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
          )}
        </section>
        <ThreadCheckHistory run={run} selectedBatch={selectedBatch} onChoose={onCheck} />
      </Modal>
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
    "The earlier evidence could not be confirmed. Choose an accepted version from History.",
  );
}
