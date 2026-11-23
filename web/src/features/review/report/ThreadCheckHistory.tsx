import { useState } from "react";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import type { RunDetail } from "../../../api/generated/contracts";
import { readFollowupResults } from "../../../api/review";
import { useResource } from "../../../hooks/useResource";
import { Pagination } from "../../../components/Pagination";
import { Loading, Notice } from "../../../components/PanelState";
import { formatTime } from "../../../lib/presentation";

export function ThreadCheckHistory({
  run,
  selectedBatch,
  onChoose,
}: {
  readonly run: RunDetail;
  readonly selectedBatch: string | undefined;
  readonly onChoose: (result: ResultSummary) => void;
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const cursor = trail.at(-1);
  const results = useResource(
    `followup-results:${run.reviewId}:${cursor ?? ""}`,
    (signal) => readFollowupResults(run.reviewId, cursor, signal),
    [{ kind: "followup", id: run.reviewId }],
  );
  return (
    <section className="report-history-list stack" aria-label="Follow-up result history">
      <h3 className="report-check-heading">Thread checks</h3>
      {results.pending && <Loading label="Loading follow-up results…" />}
      {results.error && <Notice retry={results.retry}>{results.error}</Notice>}
      {results.data?.items.length === 0 && <p className="meta">No completed follow-ups.</p>}
      {results.data?.items.map((result) => (
        <button
          type="button"
          className="quiet-button report-version"
          key={result.batchId}
          aria-pressed={selectedBatch === result.batchId}
          title={formatTime(result.completedAt)}
          onClick={() => onChoose(result)}
        >
          <span className="report-check-summary">
            {result.runId !== run.runId && <small className="meta">Another run</small>}
            <span>
              {result.concernCount} {result.concernCount === 1 ? "concern" : "concerns"} checked ·{" "}
              {result.kind === "unchanged" ? "Findings unchanged" : "Findings updated"}
            </span>
            {result.concernCount > 0 && (
              <small className="meta">
                {result.findingIds.join(", ") || "Selected threads"}
                {result.moreFindingIds && ", …"}
                {result.reasonPreview && ` · ${result.reasonPreview}`}
              </small>
            )}
          </span>
        </button>
      ))}
      <Pagination trail={trail} next={results.data?.nextCursor} onPage={setTrail} />
    </section>
  );
}
