import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { ApiResponseError } from "../../../../api/client";
import type { Question } from "../../../../api/generated/contracts/common";
import type { Form } from "../../../../api/generated/contracts/questions";
import { readQuestion } from "../../../../api/history";
import type { QuestionAddress } from "../../../../api/history";
import { Notice } from "../../../../components/PanelState";
import { useResource } from "../../../../hooks/useResource";
import { describeError } from "../../../../lib/presentation";
import { questionDraftValues } from "../../../../lib/question-draft";
import { questionDraftPrefix, questionKey } from "./question-key";
import { QuestionOptions } from "./QuestionField";
import { QuestionContent, QuestionPrompt, QuestionStatus } from "./QuestionPrompt";
import { canAnswer, needsResponse, questionResources } from "./useQuestions";
import type { Questions } from "./useQuestions";

type Observe = (element: HTMLElement, key: string) => () => void;

export function QuestionEntry({
  requestId,
  itemId,
  turnId,
  recordedStatus,
  selected,
  items,
  questions,
  observe,
}: {
  readonly requestId: string;
  readonly itemId: string;
  readonly turnId: string | null;
  readonly recordedStatus?: "answered" | "closed";
  readonly selected?: Readonly<Record<string, readonly string[]>>;
  readonly items: readonly Question[];
  readonly questions: Questions;
  readonly observe: Observe;
}) {
  // Claude transcript rows identify the assistant message and tool invocation.
  // Only a live, scoped form supplies its native turn for answering.
  const address: QuestionAddress | null = turnId === null ? null : { requestId, itemId, turnId };
  const key = address ? questionKey(address) : requestId;
  const known = address
    ? questions.byKey.get(key)
    : [...questions.byKey.values()].find(
        ({ form }) => form.requestId === requestId && form.itemId === itemId,
      );
  const { target, store, reconcile } = questions;
  const [replying, setReplying] = useState(false);
  const [inspecting, setInspecting] = useState(false);
  const focusedRead = !!address && !recordedStatus && !known && (inspecting || replying);
  const exact = useResource(
    `question-detail:${target.runId}:${target.sessionKey}:${key}:${focusedRead}`,
    async (signal) => {
      if (!focusedRead || !address) return { data: null, cursor: "" };
      try {
        return await readQuestion(target, address, signal);
      } catch (error) {
        if (error instanceof ApiResponseError && error.code === "not_found")
          return { data: null, cursor: "" };
        throw error;
      }
    },
    focusedRead ? questionResources(target) : [],
  );
  const live =
    known ?? (exact.data ? questions.stateFor(exact.data, Boolean(exact.error)) : undefined);
  const form = live?.form;
  const displayed = (items.length ? items : (form?.questions ?? [])).filter(
    (question) => !question.isSecret,
  );
  const actionable = !recordedStatus && form && !form.blocking && needsResponse(form.status);
  const reply = useRef<HTMLButtonElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const activePrompt = useRef<HTMLDivElement>(null);
  const moveToPrompt = useRef(false);
  const wasFocused = useRef(false);
  const draftAddress = form ?? address;
  const prefix = draftAddress ? questionDraftPrefix(target, draftAddress) : null;
  const hasDraft = useSyncExternalStore(
    store.subscribe,
    () => !!prefix && store.getSnapshot().entries.some((draft) => draft.key.startsWith(prefix)),
  );
  const open = Boolean(actionable && replying);
  const outcome = outcomeLabel(recordedStatus ?? known?.form.status);
  const visibleKey = form ? questionKey(form) : key;
  const watch = useQuestionAnchor(observe, visibleKey);
  const rememberFocus = useCallback((node: HTMLDivElement | null) => {
    if (!node) return;
    activePrompt.current = node;
    return () => {
      wasFocused.current = node.contains(document.activeElement);
      activePrompt.current = null;
    };
  }, []);

  useLayoutEffect(() => {
    if (open && moveToPrompt.current) {
      moveToPrompt.current = false;
      if (document.activeElement === document.body)
        activePrompt.current?.querySelector<HTMLElement>("input, select, textarea")?.focus();
    }
    if (open || !wasFocused.current) return;
    wasFocused.current = false;
    if (document.activeElement === document.body)
      (reply.current ?? body.current)?.focus({ preventScroll: true });
  }, [open]);

  useEffect(() => {
    if (exact.data) reconcile(exact.data);
  }, [exact.data, reconcile]);

  const controls = focusedRead
    ? {
        ...questions,
        refresh: () => {
          exact.retry();
          questions.refresh();
        },
        respond: async (form: Form, values: Readonly<Record<string, readonly string[]>>) => {
          await questions.respond(form, values);
          exact.retry();
        },
      }
    : questions;

  return (
    <div className="question-history" ref={body} tabIndex={-1}>
      {!open && <QuestionContent items={displayed} showChoices={false} />}
      {actionable && form && (
        <div className="question-history-action" ref={watch}>
          {open ? (
            <div ref={rememberFocus}>
              <QuestionPrompt
                form={form}
                questions={controls}
                error={live.error}
                onCancel={() => setReplying(false)}
              />
            </div>
          ) : canAnswer(form) ? (
            <button
              type="button"
              ref={reply}
              className="quiet-button"
              onClick={(event) => {
                moveToPrompt.current = document.activeElement === event.currentTarget;
                setReplying(true);
              }}
            >
              Reply
            </button>
          ) : (
            <QuestionStatus form={form} questions={controls} />
          )}
        </div>
      )}
      {outcome && <p className="meta question-outcome">{outcome}</p>}
      {!open &&
        (!displayed.length ||
          (address && !known && !recordedStatus) ||
          hasDraft ||
          displayed.some(
            (question) => question.options.length || selected?.[question.id]?.length,
          )) && (
          <details
            className="question-inspection"
            onToggle={(event) => setInspecting(event.currentTarget.open)}
          >
            <summary>Question details</summary>
            {inspecting && (
              <QuestionDetails
                items={displayed}
                selected={selected}
                form={form}
                prefix={prefix}
                store={store}
                reading={focusedRead && exact.pending}
                error={exact.error}
                outcome={known || !form ? undefined : outcomeLabel(form.status)}
                onRetry={exact.retry}
              />
            )}
          </details>
        )}
    </div>
  );
}

function QuestionDetails({
  items,
  selected,
  form,
  prefix,
  store,
  reading,
  error,
  outcome,
  onRetry,
}: {
  readonly items: readonly Question[];
  readonly selected: Readonly<Record<string, readonly string[]>> | undefined;
  readonly form: Form | undefined;
  readonly prefix: string | null;
  readonly store: Questions["store"];
  readonly reading: boolean;
  readonly error: string | undefined;
  readonly outcome: string | undefined;
  readonly onRetry: () => void;
}) {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [notice, setNotice] = useState<string>();
  return (
    <div className="question-inspection-body stack">
      {error && <Notice retry={onRetry}>{error}</Notice>}
      {reading && (
        <p className="meta" role="status">
          Reading question details…
        </p>
      )}
      {!items.length && !reading && !error && (
        <p className="meta">The original question text is unavailable.</p>
      )}
      {outcome && <p className="meta">{outcome}</p>}
      {items.map(
        (question) =>
          (question.options.length > 0 || selected?.[question.id]?.length) && (
            <div key={question.id}>
              {items.length > 1 && <strong>{question.header || question.question}</strong>}
              <QuestionOptions options={question.options} selected={selected?.[question.id]} />
            </div>
          ),
      )}
      {notice && <Notice>{notice}</Notice>}
      {!needsResponse(form?.status ?? "unavailable") &&
        snapshot.entries
          .filter((draft) => prefix && draft.key.startsWith(prefix))
          .map((draft) => (
            <div className="actions" key={draft.key}>
              <small className="meta">Unsent answer</small>
              <button
                type="button"
                onClick={() => {
                  setNotice(undefined);
                  void navigator.clipboard
                    .writeText(questionDraftValues(draft.text).join(", "))
                    .catch((error: unknown) => setNotice(describeError(error)));
                }}
              >
                Copy
              </button>
              <button
                type="button"
                onClick={() => {
                  if (window.confirm("Discard this unsent answer?"))
                    store.set(draft.key, draft.label, "");
                }}
              >
                Discard
              </button>
            </div>
          ))}
    </div>
  );
}

function outcomeLabel(status: Form["status"] | undefined) {
  if (status === "answered") return "Answered";
  if (status === "closed") return "Closed without a confirmed answer";
}

function useQuestionAnchor(observe: Observe, key: string) {
  return useCallback(
    (node: HTMLElement | null) => {
      if (node) return observe(node, key);
    },
    [observe, key],
  );
}
