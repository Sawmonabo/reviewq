import { useRef, useState } from "react";
import type { RunDetail } from "../../../api/generated/contracts";
import type { ResultSummary } from "../../../api/generated/contracts/followups";
import { readFollowupBatchAssessments } from "../../../api/review";
import { Pagination } from "../../../components/Pagination";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { readable } from "../../../lib/presentation";
import { useVisibleContent } from "./useVisibleContent";

type Props = {
  readonly run: RunDetail;
  readonly batch: ResultSummary | undefined;
  readonly onDismiss: () => void;
  readonly receipt: string | undefined;
  readonly disabled: boolean;
  readonly onOpenReceipt: (receipt: string) => void;
  readonly onOpenRun: (check: ResultSummary) => void;
  readonly onAcknowledge: (result: ResultSummary, automatic: boolean) => Promise<boolean>;
};

export function FollowupResults({
  run,
  batch,
  onDismiss,
  receipt,
  disabled,
  onOpenReceipt,
  onOpenRun,
  onAcknowledge,
}: Props) {
  const [acknowledged, setAcknowledged] = useState<ReadonlySet<string>>(() => new Set());
  if (!batch) return null;
  return (
    <Assessments
      key={batch.batchId}
      reviewId={run.reviewId}
      batch={batch}
      currentRun={run.runId}
      receipt={receipt}
      disabled={disabled || acknowledged.has(batch.batchId)}
      onOpenRun={onOpenRun}
      onOpenReceipt={onOpenReceipt}
      onClose={onDismiss}
      onAcknowledge={async (automatic = false) => {
        if (await onAcknowledge(batch, automatic))
          setAcknowledged((known) => new Set([...known, batch.batchId]));
      }}
    />
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
  readonly onOpenRun: (check: ResultSummary) => void;
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
        <button type="button" onClick={() => onOpenRun(batch)}>
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
