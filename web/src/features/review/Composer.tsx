import { useId, useState } from "react";
import { ApiResponseError } from "../../api/client";
import type { DeliveryReceipt } from "../../api/generated/contracts/delivery";
import { readReceipt, sendMessage } from "../../api/history";
import type { SessionAddress } from "../../api/history";
import { Icon } from "../../components/Icon";
import { Notice } from "../../components/PanelState";
import { useDraft } from "../../hooks/useDraft";
import type { DraftStore } from "../../lib/drafts";
import { describeError } from "../../lib/presentation";
import { submits } from "../../lib/submit-key";

type Attempt = { readonly submissionId: string; readonly dispatch: () => Promise<DeliveryReceipt>; readonly accepted?: () => void; readonly receipt?: DeliveryReceipt; readonly error?: string };
const attempts = new WeakMap<DraftStore, Map<string, Attempt>>();
const labels = {
  queued: "Queued", dispatching: "Delivering", started: "Delivered", uncertain: "Delivery uncertain", rejected: "Rejected",
} as const;
const MAX_MESSAGE = 12_000;

type Props = {
  readonly store: DraftStore;
  readonly target: SessionAddress;
  readonly draftKey: string;
  readonly label: string;
  readonly targetLabel: string;
  readonly draftLabel?: string;
  readonly send?: (text: string, submissionId: string) => Promise<DeliveryReceipt>;
  readonly onAccepted?: () => void;
  readonly disabled?: boolean;
};

export function Composer({ store, target, draftKey, label, targetLabel, draftLabel = label, send, onAccepted, disabled = false }: Props) {
  const text = useDraft(store, draftKey);
  const input = useId();
  const [managed] = useState(() => {
    const existing = attempts.get(store);
    if (existing) return existing;
    const created = new Map<string, Attempt>();
    attempts.set(store, created);
    return created;
  });
  const [last, setLast] = useState(() => managed.get(draftKey));
  const [busy, setBusy] = useState(false);
  const [rejectedEdit, setRejectedEdit] = useState(false);
  const [composition, setComposition] = useState<string>();
  const current = managed.get(draftKey) ?? last;
  const unconfirmed = Boolean(current && (!current.receipt || !["started", "rejected"].includes(current.receipt.state)));
  const chars = [...text].length;
  const available = !disabled && !busy && !unconfirmed && text.trim().length > 0 && chars <= MAX_MESSAGE;

  function remember(attempt: Attempt) {
    if (attempt.receipt && ["started", "rejected"].includes(attempt.receipt.state)) managed.delete(draftKey);
    else managed.set(draftKey, attempt);
    setLast(attempt);
  }

  function finish(attempt: Attempt, receipt: DeliveryReceipt) {
    remember({ ...attempt, receipt });
    const accepted = receipt.state !== "rejected";
    if (accepted && !attempt.receipt) {
      store.finish(draftKey, true);
      attempt.accepted?.();
    } else if (!accepted) store.finish(draftKey, false);
  }

  async function reconcile(attempt: Attempt, retry: boolean) {
    if (busy) return;
    setBusy(true);
    try {
      const result = await readReceipt(target, attempt.submissionId, AbortSignal.timeout(10_000));
      finish(attempt, result);
    } catch (error) {
      if (retry && error instanceof ApiResponseError && error.code === "not_found") {
        try {
          const receipt = await attempt.dispatch();
          finish(attempt, receipt);
        } catch (retryError) { remember({ ...attempt, error: describeError(retryError) }); }
      } else remember({ ...attempt, error: describeError(error) });
    } finally { setBusy(false); }
  }

  async function submit() {
    if (!available || managed.has(draftKey)) return;
    const captured = store.freeze(draftKey);
    if (!captured) return;
    const submissionId = crypto.randomUUID();
    const attempt: Attempt = {
      submissionId,
      dispatch: () => send?.(captured.text, submissionId) ?? sendMessage(target, { text: captured.text, submissionId }),
      ...(onAccepted ? { accepted: onAccepted } : {}),
    };
    remember(attempt);
    setBusy(true);
    try {
      const receipt = await attempt.dispatch();
      finish(attempt, receipt);
    } catch (error) {
      if (error instanceof ApiResponseError && !error.retryable) {
        managed.delete(draftKey);
        store.finish(draftKey, false);
        setLast(undefined);
        setFailure(describeError(error));
      } else {
        try {
          finish(attempt, await readReceipt(target, attempt.submissionId, AbortSignal.timeout(10_000)));
        } catch {
          remember({ ...attempt, error: describeError(error) });
        }
      }
    } finally { setBusy(false); }
  }

  const [failure, setFailure] = useState<string>();
  function edit(value: string) {
    setFailure(undefined);
    const accepted = store.set(draftKey, draftLabel, value);
    setRejectedEdit(!accepted);
  }

  return <form className="composer" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
    {current && <div className="spread composer-status" role="status">
      <span>{current.receipt ? labels[current.receipt.state] : busy ? "Submitting…" : "Delivery unconfirmed"}{current.error && ` · ${current.error}`}</span>
      {unconfirmed && <button type="button" disabled={busy} onClick={() => void reconcile(current, false)}>Check delivery</button>}
      {!current.receipt && current.error && !disabled && <button type="button" disabled={busy} onClick={() => void reconcile(current, true)}>Retry same submission</button>}
    </div>}
    {failure && <Notice>{failure}</Notice>}
    {rejectedEdit && <Notice>Draft storage full. Shorten an existing draft or free space before inserting more text.</Notice>}
    <label className="sr-only" htmlFor={input}>{label}</label>
    <textarea id={input} value={composition ?? text} rows={2} placeholder={label}
      onCompositionStart={(event) => setComposition(event.currentTarget.value)}
      onCompositionEnd={(event) => { setComposition(undefined); edit(event.currentTarget.value); }}
      onChange={(event) => { if (composition !== undefined) setComposition(event.currentTarget.value); else edit(event.currentTarget.value); }}
      onKeyDown={(event) => { if (submits(event.nativeEvent)) { event.preventDefault(); void submit(); } }} />
    <div className="spread"><span className="meta">{targetLabel}{chars > MAX_MESSAGE && ` · ${chars.toLocaleString()} / ${MAX_MESSAGE.toLocaleString()} characters`}</span>
      <button type="submit" className="primary button-label" disabled={!available}><Icon name="send" size={14} /> Send</button>
    </div>
  </form>;
}
