import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { ApiResponseError } from "../../api/client";
import type { Receipt } from "../../api/generated/contracts/launch";
import { launchReceipt } from "../../api/launch";
import { Modal } from "../../components/Modal";
import { Empty, Notice } from "../../components/PanelState";
import { describeError } from "../../lib/presentation";
import type { PendingLaunch } from "./pending-launches";
import { pendingLaunches, settleLaunchReceipt, watchPendingLaunches } from "./pending-launches";

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
}: {
  readonly item: PendingLaunch;
  readonly onOpen: Open;
}) {
  const [result, setResult] = useState<Receipt>();
  const [notice, setNotice] = useState<string>();
  const [busy, setBusy] = useState(false);
  const request = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => request.current?.abort(), []);

  async function reconcile() {
    if (request.current) return;
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setNotice(undefined);
    try {
      const receipt = await launchReceipt(item.reviewId, item.submissionId, controller.signal);
      if (controller.signal.aborted) return;
      settleLaunchReceipt(receipt);
      setResult(receipt);
    } catch (error) {
      if (!controller.signal.aborted) {
        setNotice(
          error instanceof ApiResponseError && error.code === "not_found"
            ? "The service has no saved receipt for this submission yet."
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
      ) : result?.state === "rejected" ? (
        <p role="status">The launch was rejected.</p>
      ) : (
        <>
          <p role="status">Start unconfirmed</p>
          <button type="button" disabled={busy} onClick={() => void reconcile()}>
            Check saved launch receipt
          </button>
        </>
      )}
    </article>
  );
}
