import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ApiResponseError } from "../../api/client";
import type { Receipt } from "../../api/generated/contracts/launch";
import { launchReceipt, startRun } from "../../api/launch";
import { Modal } from "../../components/Modal";
import { Empty, Notice } from "../../components/PanelState";
import { describeError } from "../../lib/presentation";
import type { PendingLaunch } from "./pending-launches";
import {
  pendingLaunches,
  settleLaunchReceipt,
  settleLaunchRefusal,
  watchPendingLaunches,
} from "./pending-launches";

type Open = (review: string, run: string) => void;

export function LaunchReceipts({
  onOpen,
  onClose,
}: {
  readonly onOpen: Open;
  readonly onClose: () => void;
}) {
  const pending = useSyncExternalStore(watchPendingLaunches, pendingLaunches);
  const [visible] = useState(() => pending);
  return (
    <Modal title="Pending launches" onClose={onClose}>
      <div className="stack">
        {visible.length === 0 && <Empty title="No pending launches" />}
        {visible.map((item) => (
          <LaunchReceiptCard key={item.submissionId} item={item} onOpen={onOpen} />
        ))}
      </div>
    </Modal>
  );
}

export function LaunchReceiptCard({
  item,
  onOpen,
  onReceipt,
  onNoReceipt,
  initialReceipt,
  initialNotice,
}: {
  readonly item: PendingLaunch;
  readonly onOpen: Open;
  readonly onReceipt?: (receipt: Receipt) => void;
  readonly onNoReceipt?: () => void;
  readonly initialReceipt?: Receipt | undefined;
  readonly initialNotice?: string | undefined;
}) {
  const [result, setResult] = useState(initialReceipt);
  const [notice, setNotice] = useState(initialNotice);
  const [busy, setBusy] = useState(false);
  const [missing, setMissing] = useState(false);
  const [refused, setRefused] = useState(false);
  const request = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => request.current?.abort(), []);

  function resolved(receipt: Receipt) {
    setMissing(false);
    onReceipt?.(receipt);
    settleLaunchReceipt(receipt);
    setResult(receipt);
  }

  async function reconcile(retry: boolean) {
    if (request.current || (retry && (!missing || !item.request))) return;
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setNotice(undefined);
    try {
      const receipt =
        retry && item.request
          ? await startRun(item.request)
          : await launchReceipt(item.reviewId, item.submissionId, controller.signal);
      if (!controller.signal.aborted) resolved(receipt);
      else settleLaunchReceipt(receipt);
    } catch (error) {
      if (retry && settleLaunchRefusal(item, error)) setRefused(true);
      if (!controller.signal.aborted) {
        const noReceipt = !retry && error instanceof ApiResponseError && error.code === "not_found";
        setMissing(noReceipt);
        if (noReceipt && !item.request) onNoReceipt?.();
        setNotice(
          noReceipt
            ? "The service has no saved receipt for this submission."
            : describeError(error),
        );
      }
    } finally {
      request.current = undefined;
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  const run = result?.state === "confirmed" ? result.runId : null;
  return (
    <article className="surface-card stack">
      <strong>{item.title}</strong>
      <span className="meta">
        Submission <code>{item.submissionId}</code>
      </span>
      {notice && <Notice>{notice}</Notice>}
      {run ? (
        <button type="button" onClick={() => onOpen(item.reviewId, run)}>
          Open confirmed run
        </button>
      ) : refused || result?.state === "rejected" ? (
        <p role="status">The launch was rejected.</p>
      ) : (
        <>
          <p role="status">Start unconfirmed</p>
          <button type="button" disabled={busy} onClick={() => void reconcile(false)}>
            Check saved launch receipt
          </button>
          {missing && item.request && (
            <button type="button" disabled={busy} onClick={() => void reconcile(true)}>
              Retry the same submission
            </button>
          )}
          {missing && !item.request && (
            <p className="meta">Prepare this review to retry using the same submission.</p>
          )}
        </>
      )}
    </article>
  );
}
