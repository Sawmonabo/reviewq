import { useRef, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import { readFollowupBatchAssessments, readFollowupResults } from "../../../api/review";
import { Pagination } from "../../../components/Pagination";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { formatTime, readable } from "../../../lib/presentation";
import { saveValue } from "../../../lib/view-preferences";
import { useVisibleContent } from "./useVisibleContent";

type Props = {
  readonly run: RunDetail;
  readonly latest: ResultSummary | null | undefined;
  readonly open: boolean;
  readonly receipt: string | undefined;
  readonly disabled: boolean;
  readonly onOpenReceipt: (receipt: string) => void;
  readonly onOpenRun: (run: string) => void;
  readonly onChoose: (result: ResultSummary) => void;
  readonly onAcknowledge: (result: ResultSummary, automatic: boolean) => Promise<boolean>;
};

export function FollowupResults({
  run,
  latest,
  open,
  receipt,
  disabled,
  onOpenReceipt,
  onOpenRun,
  onChoose,
  onAcknowledge,
}: Props) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [choice, setChoice] = useState<ResultSummary>();
  const [dismissed, setDismissed] = useState<string>();
  const [acknowledged, setAcknowledged] = useState<ReadonlySet<string>>(() => new Set());
  const results = useResource(
    `followup-results:${run.reviewId}:${open}:${trail.at(-1) ?? ""}`,
    (signal) =>
      open
        ? readFollowupResults(run.reviewId, trail.at(-1), signal)
        : Promise.resolve({ data: null, cursor: "" }),
    open ? [{ kind: "followup", id: run.reviewId }] : [],
  );
  const unchanged =
    latest?.kind === "unchanged" &&
    latest.runId === run.runId &&
    receipt &&
    latest.receiptId === receipt &&
    latest.batchId !== dismissed
      ? latest
      : undefined;
  const batch = choice ?? unchanged;

  return (
    <>
      {open && (
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
              aria-pressed={batch?.batchId === result.batchId}
              onClick={() => {
                setChoice(result);
                onChoose(result);
              }}
            >
              <span>
                {result.kind === "unchanged" ? "Findings unchanged" : "Completed follow-up"}
                {result.batchId === latest?.batchId && " · Latest check"}
              </span>
              <time className="meta" dateTime={result.completedAt}>
                {formatTime(result.completedAt)}
              </time>
            </button>
          ))}
          <Pagination trail={trail} next={results.data?.nextCursor} onPage={setTrail} />
        </section>
      )}
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
          onClose={() => {
            setDismissed(batch.batchId);
            setChoice(undefined);
          }}
          onAcknowledge={async (automatic = false) => {
            if (await onAcknowledge(batch, automatic))
              setAcknowledged((known) => new Set([...known, batch.batchId]));
          }}
        />
      )}
    </>
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
  onClose,
}: {
  readonly reviewId: string;
  readonly batch: ResultSummary;
  readonly currentRun: string;
  readonly receipt: string | undefined;
  readonly disabled: boolean;
  readonly onOpenReceipt: (receipt: string) => void;
  readonly onOpenRun: (run: string) => void;
  readonly onAcknowledge: (automatic?: boolean) => Promise<void>;
  readonly onClose: () => void;
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [readPages, setReadPages] = useState<ReadonlyMap<string | undefined, boolean>>(
    () => new Map(),
  );
  const content = useRef<HTMLDivElement>(null);
  const assessments = useResource(
    `assessments:${reviewId}:${batch.batchId}:${trail.at(-1) ?? ""}`,
    (signal) => readFollowupBatchAssessments(reviewId, batch.batchId, trail.at(-1), signal),
  );
  const data = assessments.data;
  const cursor = trail.at(-1);
  const exactAssessment = Boolean(
    data?.items.some(
      (item) => item.batchId === batch.batchId && item.receiptId === batch.receiptId,
    ),
  );
  const matching = currentRun === batch.runId && receipt && receipt === batch.receiptId;
  const priorCursors = trail.length > 0 ? [undefined, ...trail.slice(0, -1)] : [];
  const eligible = Boolean(data?.throughBatchId === batch.batchId);
  const ready = Boolean(
    matching &&
    eligible &&
    data &&
    data.nextCursor === null &&
    priorCursors.every((cursor) => readPages.has(cursor)) &&
    readPages.has(cursor) &&
    (readPages.get(cursor) || priorCursors.some((cursor) => readPages.get(cursor))),
  );
  useVisibleContent(
    content,
    eligible ? JSON.stringify([batch.batchId, cursor]) : undefined,
    (identity) => {
      if (identity !== JSON.stringify([batch.batchId, cursor])) return;
      setReadPages((pages) => {
        if (pages.get(cursor) === exactAssessment) return pages;
        return new Map(pages).set(cursor, exactAssessment);
      });
      if (
        matching &&
        data?.nextCursor === null &&
        !disabled &&
        priorCursors.every((position) => readPages.has(position)) &&
        (exactAssessment || priorCursors.some((position) => readPages.get(position)))
      )
        void onAcknowledge(true);
    },
  );
  function turnPage(next: readonly string[]) {
    setTrail(next);
  }
  return (
    <div className="surface-card stack" aria-label="Selected follow-up assessments">
      <div className="spread">
        <h3>
          {batch.kind === "unchanged"
            ? "Discussion outcomes · Findings unchanged"
            : "Discussion outcomes"}
        </h3>
        <button type="button" className="quiet-button" onClick={onClose}>
          Close thread check
        </button>
      </div>
      {batch.checkedSha && (
        <p className="meta">
          Checked revision · <code>{batch.checkedSha}</code>
        </p>
      )}
      {assessments.error && <Notice retry={assessments.retry}>{assessments.error}</Notice>}
      {assessments.pending && <Loading label="Reading exact follow-up assessments…" />}
      {data && !data.items.length && <p className="meta">No assessments for this thread check.</p>}
      {data && (
        <div className="stack" ref={content}>
          {data.items.map((item) => (
            <article key={`${item.batchId}:${item.discussionId}`} className="stack">
              <div className="spread">
                <strong>
                  {item.findingIds.length > 0 ? item.findingIds.join(", ") : "Discussion"}
                </strong>
                <span className="badge">{readable(item.conclusion)}</span>
              </div>
              <p>{item.reason}</p>
              <p className="meta">
                {readable(item.nextAction)} · <code>{item.discussionId}</code>
              </p>
            </article>
          ))}
        </div>
      )}
      <Pagination trail={trail} next={data?.nextCursor} onPage={turnPage} />
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
