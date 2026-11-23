import { useId, useRef, useState, useSyncExternalStore } from "react";
import { ApiResponseError } from "../../api/client";
import type { DeliveryReceipt } from "../../api/generated/contracts/delivery";
import { readReceipt, sendMessage } from "../../api/history";
import type { SessionAddress } from "../../api/history";
import { Icon } from "../../components/Icon";
import { Notice } from "../../components/PanelState";
import { useDraft } from "../../hooks/useDraft";
import { useResource } from "../../hooks/useResource";
import { useWorkspaceRecovery } from "../../hooks/useWorkspaceRecovery";
import { isWorkspaceConnectionFailure } from "../../hooks/useWorkspaceResource";
import type { DraftStore } from "../../lib/drafts";
import { describeError } from "../../lib/presentation";
import { submits } from "../../lib/submit-key";
import { deliveryLabels } from "./delivery-labels";
import "./composer.css";

type Attempt = {
  readonly submissionId: string;
  readonly dispatch?: () => Promise<DeliveryReceipt>;
  readonly accepted?: () => void;
  readonly receipt?: DeliveryReceipt;
  readonly error?: string;
  readonly lookupInterrupted?: boolean;
};
const attempts = new WeakMap<DraftStore, SubmissionAttempts>();
const MAX_MESSAGE = 12_000;
const RECEIPT_TIMEOUT_MS = 10_000;

type Props = {
  readonly store: DraftStore;
  readonly target: SessionAddress;
  readonly draftKey: string;
  readonly label: string;
  readonly placeholder?: string;
  readonly targetLabel: string;
  readonly targetTitle?: string;
  readonly draftLabel?: string;
  readonly send?: (text: string, submissionId: string) => Promise<DeliveryReceipt>;
  readonly onAccepted?: () => void;
  readonly disabled?: boolean;
  readonly deliveryPending?: boolean | undefined;
};

export function Composer(props: Props) {
  return <MessageComposer key={props.draftKey} {...props} />;
}

function MessageComposer({
  store,
  target,
  draftKey,
  label,
  placeholder = label,
  targetLabel,
  targetTitle,
  draftLabel = label,
  send,
  onAccepted,
  disabled = false,
  deliveryPending = false,
}: Props) {
  const text = useDraft(store, draftKey);
  const input = useId();
  const [managed] = useState(() => {
    const existing = attempts.get(store);
    if (existing) return existing;
    const created = new SubmissionAttempts();
    attempts.set(store, created);
    return created;
  });
  const [busy, setBusy] = useState(false);
  const checking = useRef(false);
  const [rejectedEdit, setRejectedEdit] = useState(false);
  const [composition, setComposition] = useState<string>();
  const [failure, setFailure] = useState<string>();
  const current = useSyncExternalStore(managed.subscribe, () => managed.get(draftKey));
  const recoverable = current?.receipt?.state === "uncertain" || Boolean(current?.error);
  const observing =
    current && (current.receipt || current.error) ? current.submissionId : undefined;
  useResource(
    JSON.stringify(["delivery-receipt", target.runId, target.sessionKey, observing]),
    async (signal) => {
      const attempt = managed.get(draftKey);
      if (!observing || attempt?.submissionId !== observing) return { data: null, cursor: "" };
      try {
        const { data, cursor } = await readReceipt(
          target,
          observing,
          AbortSignal.any([signal, AbortSignal.timeout(RECEIPT_TIMEOUT_MS)]),
        );
        signal.throwIfAborted();
        finish(data);
        return { data: null, cursor };
      } catch (error) {
        if (!signal.aborted) rememberError(attempt, error, error);
        throw error;
      }
    },
    observing ? [{ kind: "session", id: target.sessionKey }] : [],
  );
  const chars = [...text].length;
  const available =
    !disabled && !busy && !current && text.trim().length > 0 && chars <= MAX_MESSAGE;

  function finish(receipt: DeliveryReceipt) {
    const attempt = managed.get(draftKey);
    if (
      attempt?.submissionId !== receipt.submissionId ||
      (attempt.receipt && attempt.receipt.updatedAt > receipt.updatedAt)
    )
      return;

    if (receipt.state === "rejected") setFailure("Delivery rejected.");
    if (!attempt.receipt) {
      const accepted = receipt.state !== "rejected";
      store.finish(draftKey, accepted);
      if (accepted) attempt.accepted?.();
    }
    if (receipt.state === "rejected" || receipt.state === "started")
      managed.set(draftKey, undefined);
    else if (attempt.receipt?.state !== receipt.state || attempt.error)
      managed.set(draftKey, { submissionId: attempt.submissionId, receipt });
  }

  function rememberError(attempt: Attempt, error: unknown, lookupError: unknown) {
    if (managed.get(draftKey) !== attempt) return;
    managed.set(draftKey, {
      ...attempt,
      error: describeError(error),
      lookupInterrupted: receiptUnavailable(lookupError),
    });
  }

  async function reconcile(attempt: Attempt, retry: boolean) {
    if (busy || checking.current || managed.get(draftKey)?.submissionId !== attempt.submissionId)
      return;
    checking.current = true;
    setFailure(undefined);
    setBusy(true);
    try {
      const { data } = await readReceipt(
        target,
        attempt.submissionId,
        AbortSignal.timeout(RECEIPT_TIMEOUT_MS),
      );
      finish(data);
    } catch (error) {
      if (
        retry &&
        attempt.dispatch &&
        managed.get(draftKey) === attempt &&
        error instanceof ApiResponseError &&
        error.code === "not_found"
      ) {
        try {
          finish(await attempt.dispatch());
        } catch (retryError) {
          rememberError(attempt, retryError, retryError);
        }
      } else {
        rememberError(attempt, error, error);
      }
    } finally {
      checking.current = false;
      setBusy(false);
    }
  }

  async function submit() {
    if (!available || managed.get(draftKey)) return;
    const captured = store.freeze(draftKey);
    if (!captured) return;
    setFailure(undefined);
    const submissionId = crypto.randomUUID();
    const dispatch = () =>
      send?.(captured.text, submissionId) ??
      sendMessage(target, { text: captured.text, submissionId });
    const attempt: Attempt = {
      submissionId,
      dispatch,
      ...(onAccepted ? { accepted: onAccepted } : {}),
    };
    managed.set(draftKey, attempt);
    setBusy(true);
    try {
      finish(await dispatch());
    } catch (error) {
      if (error instanceof ApiResponseError && !error.retryable) {
        managed.set(draftKey, undefined);
        store.finish(draftKey, false);
        setFailure(describeError(error));
      } else {
        try {
          const { data } = await readReceipt(
            target,
            attempt.submissionId,
            AbortSignal.timeout(RECEIPT_TIMEOUT_MS),
          );
          finish(data);
        } catch (lookupError) {
          rememberError(attempt, error, lookupError);
        }
      }
    } finally {
      setBusy(false);
    }
  }

  useWorkspaceRecovery(
    current?.lookupInterrupted
      ? { kind: "receipt", label: "Delivery receipt", retry: () => void reconcile(current, false) }
      : undefined,
  );
  const sendLabel = "Send to review agent";
  const pendingLabel =
    current && !current.receipt && !recoverable
      ? "Sending…"
      : !recoverable && (current || deliveryPending)
        ? "Waiting for agent…"
        : undefined;
  function edit(value: string) {
    setFailure(undefined);
    const accepted = store.set(draftKey, draftLabel, value);
    setRejectedEdit(!accepted);
  }

  return (
    <div className={send ? "feedback-composer-shell" : "agent-composer-shell"}>
      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        {current && recoverable && (
          <div className="spread composer-status" role="status">
            <span>
              {current.receipt?.state === "uncertain"
                ? deliveryLabels.uncertain
                : current.receipt
                  ? "Could not check delivery"
                  : "Delivery unconfirmed"}
              {current.error && ` · ${current.error}`}
            </span>
            <button type="button" disabled={busy} onClick={() => void reconcile(current, false)}>
              Check delivery
            </button>
            {!current.receipt && current.error && current.dispatch && !disabled && (
              <button type="button" disabled={busy} onClick={() => void reconcile(current, true)}>
                Retry same submission
              </button>
            )}
          </div>
        )}
        {failure && <Notice>{failure}</Notice>}
        {rejectedEdit && (
          <Notice>
            Draft storage full. Shorten an existing draft or free space before inserting more text.
          </Notice>
        )}
        <label className={send ? "feedback-label" : "sr-only"} htmlFor={input}>
          {label}
        </label>
        <textarea
          id={input}
          value={composition ?? text}
          rows={send ? 3 : 2}
          placeholder={placeholder}
          onCompositionStart={(event) => setComposition(event.currentTarget.value)}
          onCompositionEnd={(event) => {
            setComposition(undefined);
            edit(event.currentTarget.value);
          }}
          onChange={(event) => {
            if (composition !== undefined) setComposition(event.currentTarget.value);
            else edit(event.currentTarget.value);
          }}
          onKeyDown={(event) => {
            if (submits(event.nativeEvent)) {
              event.preventDefault();
              void submit();
            }
          }}
        />
        <div className="composer-bottom">
          <span className="composer-destination" title={targetTitle}>
            {targetLabel}
            {pendingLabel && <span role="status"> · {pendingLabel}</span>}
            <span className="composer-shortcut"> · Enter to send · Shift+Enter for a new line</span>
            {chars > MAX_MESSAGE &&
              ` · ${chars.toLocaleString()} / ${MAX_MESSAGE.toLocaleString()} characters`}
          </span>
          <button
            type="submit"
            className={send ? "primary feedback-send" : "composer-send"}
            aria-label={sendLabel}
            title={busy ? "Submitting…" : sendLabel}
            aria-busy={busy}
            disabled={!available}
          >
            {send ? busy ? "Submitting…" : sendLabel : <Icon name="send" size={16} />}
          </button>
        </div>
      </form>
    </div>
  );
}

class SubmissionAttempts {
  readonly #attempts = new Map<string, Attempt>();
  readonly #listeners = new Set<() => void>();

  get(key: string) {
    return this.#attempts.get(key);
  }

  set(key: string, attempt: Attempt | undefined) {
    if (attempt) this.#attempts.set(key, attempt);
    else this.#attempts.delete(key);
    for (const listener of this.#listeners) listener();
  }

  subscribe = (listener: () => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };
}

function receiptUnavailable(error: unknown) {
  return (
    isWorkspaceConnectionFailure(error) ||
    (error instanceof DOMException && error.name === "TimeoutError")
  );
}
