import { useEffect, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import { readFollowupAssessments, readFollowupResults } from "../../../api/review";
import { Pagination } from "../../../components/Pagination";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { formatTime, readable } from "../../../lib/presentation";
import { saveValue } from "../../../lib/view-preferences";

type Props = {
  readonly run: RunDetail;
  readonly latest: ResultSummary | null | undefined;
  readonly receipt: string | undefined;
  readonly disabled: boolean;
  readonly onOpenReceipt: (receipt: string) => void;
  readonly onOpenRun: (run: string) => void;
  readonly onAcknowledge: (result: ResultSummary) => Promise<boolean>;
};

export function FollowupResults({
  run,
  latest,
  receipt,
  disabled,
  onOpenReceipt,
  onOpenRun,
  onAcknowledge,
}: Props) {
  const [open, setOpen] = useState(false);
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [choice, setChoice] = useState<ResultSummary>();
  const [acknowledged, setAcknowledged] = useState<ReadonlySet<string>>(() => new Set());
  const results = useResource(
    `followup-results:${run.reviewId}:${open}:${trail.at(-1) ?? ""}`,
    (signal) =>
      open
        ? readFollowupResults(run.reviewId, trail.at(-1), signal)
        : Promise.resolve({ data: null, cursor: "" }),
    open ? [{ kind: "followup", id: run.reviewId }] : [],
  );
  const batch = choice ?? (latest?.runId === run.runId ? latest : undefined);

  return (
    <details className="surface-card stack" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        <strong>Follow-up results</strong>
        {batch?.kind === "unchanged" ? " · Findings unchanged" : ""}
      </summary>
      {open && (
        <section className="stack" aria-label="Follow-up result history">
          {results.pending && <Loading label="Loading follow-up results…" />}
          {results.error && <Notice retry={results.retry}>{results.error}</Notice>}
          {results.data?.items.length === 0 && <p className="meta">No completed follow-ups.</p>}
          {results.data?.items.map((result) => (
            <div key={result.batchId} className="spread">
              <div>
                <strong>
                  {result.kind === "unchanged" ? "Findings unchanged" : "Completed follow-up"}
                </strong>
                <div className="meta">
                  {formatTime(result.completedAt)}
                  {result.checkedSha && ` · ${result.checkedSha.slice(0, 12)}`}
                </div>
              </div>
              <button
                type="button"
                aria-pressed={batch?.batchId === result.batchId}
                onClick={() => setChoice(result)}
              >
                Read assessments
              </button>
            </div>
          ))}
          <Pagination trail={trail} next={results.data?.nextCursor} onPage={setTrail} />
          {batch && (
            <Assessments
              key={batch.batchId}
              reviewId={run.reviewId}
              batch={batch}
              currentRun={run.runId}
              receipt={receipt}
              disabled={disabled || acknowledged.has(batch.batchId)}
              onOpenRun={onOpenRun}
              onOpenReceipt={onOpenReceipt}
              onAcknowledge={async () => {
                if (await onAcknowledge(batch))
                  setAcknowledged((known) => new Set([...known, batch.batchId]));
              }}
            />
          )}
        </section>
      )}
    </details>
  );
}

function Assessments({
  reviewId,
  batch,
  currentRun,
  receipt,
  disabled,
  onOpenReceipt,
  onOpenRun,
  onAcknowledge,
}: {
  readonly reviewId: string;
  readonly batch: ResultSummary;
  readonly currentRun: string;
  readonly receipt: string | undefined;
  readonly disabled: boolean;
  readonly onOpenReceipt: (receipt: string) => void;
  readonly onOpenRun: (run: string) => void;
  readonly onAcknowledge: () => Promise<void>;
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [rendered, setRendered] = useState<{
    through: string;
    count: number;
    complete: boolean;
    exact: boolean;
  }>({ through: batch.batchId, count: 0, complete: false, exact: false });
  const assessments = useResource(
    `assessments:${reviewId}:${batch.batchId}:${trail.at(-1) ?? ""}`,
    (signal) => readFollowupAssessments(reviewId, batch.batchId, trail.at(-1), signal),
  );
  const data = assessments.data;
  const depth = trail.length + 1;

  useEffect(() => {
    if (!data || data.throughBatchId !== batch.batchId) return;
    setRendered((previous) => {
      if (previous.through !== batch.batchId || depth > previous.count + 1) return previous;
      const exact =
        previous.exact ||
        data.items.some(
          (item) => item.batchId === batch.batchId && item.receiptId === batch.receiptId,
        );
      if (previous.count >= depth && previous.exact === exact) return previous;
      return {
        through: batch.batchId,
        count: Math.max(depth, previous.count),
        complete: data.nextCursor === null || previous.complete,
        exact,
      };
    });
  }, [batch.batchId, batch.receiptId, data, depth]);

  const matching = currentRun === batch.runId && receipt && receipt === batch.receiptId;
  const ready = Boolean(
    matching &&
    data?.nextCursor === null &&
    rendered.through === batch.batchId &&
    rendered.complete &&
    rendered.exact,
  );
  return (
    <div className="surface-card stack" aria-label="Selected follow-up assessments">
      <div className="spread">
        <h3>
          {batch.kind === "unchanged"
            ? "Discussion outcomes · Findings unchanged"
            : "Discussion outcomes"}
        </h3>
        <code>{batch.batchId}</code>
      </div>
      {batch.checkedSha && (
        <p className="meta">
          Checked revision · <code>{batch.checkedSha}</code>
        </p>
      )}
      {assessments.error && <Notice retry={assessments.retry}>{assessments.error}</Notice>}
      {assessments.pending && <Loading label="Reading exact follow-up assessments…" />}
      {data && !data.items.length && (
        <p className="meta">No unacknowledged assessments on this page.</p>
      )}
      {data?.items.map((item) => (
        <article key={`${item.batchId}:${item.discussionId}`} className="stack">
          <div className="spread">
            <strong>
              Discussion <code>{item.discussionId}</code>
            </strong>
            <span className="badge">{readable(item.conclusion)}</span>
          </div>
          <p>{item.reason}</p>
          <p className="meta">
            {readable(item.nextAction)}
            {item.findingIds.length > 0 && ` · ${item.findingIds.join(", ")}`}
            {item.batchId !== batch.batchId && ` · Earlier batch ${item.batchId}`}
          </p>
        </article>
      ))}
      <Pagination trail={trail} next={data?.nextCursor} onPage={setTrail} />
      {currentRun !== batch.runId && (
        <button
          type="button"
          onClick={() => {
            if (batch.receiptId) saveValue(`report.receipt.${batch.runId}`, batch.receiptId);
            onOpenRun(batch.runId);
          }}
        >
          Open the run for this result
        </button>
      )}
      {currentRun === batch.runId && batch.receiptId && receipt !== batch.receiptId && (
        <button
          type="button"
          onClick={() => {
            if (batch.receiptId) onOpenReceipt(batch.receiptId);
          }}
        >
          Open the accepted report for this result
        </button>
      )}
      {matching && (
        <button type="button" disabled={disabled || !ready} onClick={() => void onAcknowledge()}>
          Mark this follow-up result viewed
        </button>
      )}
      {matching && !ready && !disabled && Boolean(data?.nextCursor) && (
        <p className="meta">
          Read through the displayed assessment pages to acknowledge this result.
        </p>
      )}
    </div>
  );
}
