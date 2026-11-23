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
import { questionDraftPrefix, questionKey, questionValue } from "./question-key";
import { QuestionContent, QuestionPrompt, QuestionStatus } from "./QuestionPrompt";
import { canAnswer, needsResponse, questionResources } from "./useQuestions";
import type { Questions } from "./useQuestions";

type Observe = (element: HTMLElement, key: string) => () => void;

export function QuestionEntry({
  address,
  items,
  questions,
  observe,
}: {
  readonly address: QuestionAddress;
  readonly items: readonly Question[];
  readonly questions: Questions;
  readonly observe: Observe;
}) {
  const { requestId, turnId, itemId } = address;
  const key = questionKey(address);
  const known = questions.byKey.get(key);
  const { target, store, reconcile } = questions;
  const [replying, setReplying] = useState(false);
  const [inspecting, setInspecting] = useState(false);
  const focusedRead = !known && (inspecting || replying);
  const exact = useResource(
    `question-detail:${target.runId}:${target.sessionKey}:${key}:${focusedRead}`,
    async (signal) => {
      if (!focusedRead) return { data: null, cursor: "" };
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
  const actionable = form && !form.blocking && needsResponse(form.status);
  const reply = useRef<HTMLButtonElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const activePrompt = useRef<HTMLDivElement>(null);
  const moveToPrompt = useRef(false);
  const wasFocused = useRef(false);
  const prefix = questionDraftPrefix(target, address);
  const hasDraft = useSyncExternalStore(store.subscribe, () =>
    store.getSnapshot().entries.some((draft) => draft.key.startsWith(prefix)),
  );
  const open = Boolean(actionable && replying);
  const outcome = known && outcomeLabel(known.form.status);
  const watch = useCallback(
    (node: HTMLElement | null) => {
      if (node) return observe(node, questionKey({ requestId, turnId, itemId }));
    },
    [itemId, observe, requestId, turnId],
  );
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
        respond: async (form: Form, values: Readonly<Record<string, string>>) => {
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
          !known ||
          hasDraft ||
          displayed.some((question) => question.options.length > 0)) && (
          <details
            className="question-inspection"
            onToggle={(event) => setInspecting(event.currentTarget.open)}
          >
            <summary>Question details</summary>
            {inspecting && (
              <QuestionDetails
                items={displayed}
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
  form,
  prefix,
  store,
  reading,
  error,
  outcome,
  onRetry,
}: {
  readonly items: readonly Question[];
  readonly form: Form | undefined;
  readonly prefix: string;
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
          question.options.length > 0 && (
            <div key={question.id}>
              {items.length > 1 && <strong>{question.header || question.question}</strong>}
              <ul>
                {question.options.map((option) => (
                  <li key={option.label}>
                    {option.label}
                    {option.description && ` · ${option.description}`}
                  </li>
                ))}
              </ul>
            </div>
          ),
      )}
      {notice && <Notice>{notice}</Notice>}
      {!needsResponse(form?.status ?? "unavailable") &&
        snapshot.entries
          .filter((draft) => draft.key.startsWith(prefix))
          .map((draft) => (
            <div className="actions" key={draft.key}>
              <small className="meta">Unsent answer</small>
              <button
                type="button"
                onClick={() => {
                  setNotice(undefined);
                  void navigator.clipboard
                    .writeText(questionValue(draft.text))
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

function outcomeLabel(status: Form["status"]) {
  if (status === "answered") return "Answered";
  if (status === "closed") return "Closed without a confirmed answer";
}
