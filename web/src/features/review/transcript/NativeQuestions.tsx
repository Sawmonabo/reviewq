import { useEffect, useId, useRef, useState, useSyncExternalStore } from "react";
import type { Form as NativeForm, Question } from "../../../api/generated/contracts/questions";
import { answerQuestion, readQuestions } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Notice } from "../../../components/PanelState";
import { Icon } from "../../../components/Icon";
import { useDraft } from "../../../hooks/useDraft";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { describeError } from "../../../lib/presentation";
import { submits } from "../../../lib/submit-key";

export function NativeQuestions({
  target,
  store,
  reviewTitle,
  sessionName,
  child,
  onTerminal,
}: {
  readonly target: SessionAddress;
  readonly store: DraftStore;
  readonly reviewTitle: string;
  readonly sessionName: string;
  readonly child: boolean;
  readonly onTerminal: () => void;
}) {
  const questions = useResource(
    `questions:${target.runId}:${target.sessionKey}`,
    (signal) => readQuestions(target, signal),
    [
      { kind: "session", id: target.sessionKey },
      { kind: "history", id: target.sessionKey },
      { kind: "run", id: target.runId },
    ],
  );
  if (questions.error) return <Notice retry={questions.retry}>{questions.error}</Notice>;
  if (questions.data && !questions.data.available)
    return (
      <div className="native-questions muted">
        Native form details unavailable.{" "}
        <button type="button" onClick={onTerminal}>
          Open in terminal
        </button>
      </div>
    );
  if (!questions.data?.items.length) return null;
  return (
    <div className="native-questions stack" aria-label="Native questions">
      {questions.data.items.map((form) => (
        <QuestionForm
          key={JSON.stringify([form.requestId, form.turnId, form.itemId])}
          form={form}
          target={target}
          store={store}
          label={reviewTitle}
          destination={`To ${sessionName} · ${child ? "Subagent" : "Lead review"}`}
          onTerminal={onTerminal}
          onUpdate={questions.retry}
        />
      ))}
    </div>
  );
}

function QuestionForm({
  form,
  target,
  store,
  label,
  destination,
  onTerminal,
  onUpdate,
}: {
  readonly form: NativeForm;
  readonly target: SessionAddress;
  readonly store: DraftStore;
  readonly label: string;
  readonly destination: string;
  readonly onTerminal: () => void;
  readonly onUpdate: () => void;
}) {
  const [outcome, setOutcome] = useState<{
    state: NativeForm["status"];
    original: NativeForm["status"];
  }>();
  const [notice, setNotice] = useState<string>();
  const [minimized, setMinimized] = useState(false);
  const bodyId = useId();
  const heading = useRef<HTMLButtonElement>(null);
  const submission = useRef<string | undefined>(undefined);
  const busy = useRef(false);
  const element = useRef<HTMLFormElement>(null);
  const status = outcome?.original === form.status ? outcome.state : form.status;
  const answered = status === "answered";
  const reviewable = answered ? form.questions.filter((question) => !question.isSecret) : [];
  const includesSecret = form.questions.some((question) => question.isSecret);
  const open =
    status === "pending" &&
    form.questions.length > 0 &&
    form.answerable &&
    (!includesSecret || (form.mode === "native" && form.blocking));
  const readOnly =
    status === "pending" && !open && form.questions.some((question) => !question.isSecret);
  const draftName = `Question · ${label}`;
  const prefix = [target.runId, target.sessionKey, form.requestId, form.turnId, form.itemId];
  const title = {
    pending: "Answer requested",
    submitting: "Sending answer…",
    answered: "Answered",
    closed: "Closed without a confirmed answer",
    unavailable: "Answer unavailable",
    uncertain: "Answer delivery unconfirmed",
  }[status];

  async function submit() {
    if (!open || busy.current) return;
    const entries = store.getSnapshot().entries;
    const selected = form.questions.map((question) => {
      const value = question.isSecret
        ? (secretInput(element.current, question.id)?.value ?? "")
        : questionValue(
            entries.find((entry) => entry.key === answerKey(prefix, question.id))?.text ?? "",
          );
      return [question.id, value] as const;
    });
    const invalid = selected.find(
      ([, value]) => !value.trim() || new TextEncoder().encode(value).length > 8192,
    );
    if (invalid) {
      setNotice("Enter an answer of up to 8 KiB for each required question.");
      const field = [...(element.current?.querySelectorAll<HTMLElement>("fieldset") ?? [])].find(
        (entry) => entry.dataset.question === invalid[0],
      );
      field?.querySelector<HTMLElement>("input, select, button, textarea")?.focus();
      return;
    }
    clearSecrets(element.current);
    const id = submission.current ?? crypto.randomUUID();
    submission.current = id;
    busy.current = true;
    setNotice(undefined);
    setOutcome({ state: "submitting", original: form.status });
    const locked = form.questions
      .filter((question) => !question.isSecret)
      .map((question) => answerKey(prefix, question.id));
    for (const key of locked) store.freeze(key);
    try {
      const result = await answerQuestion(target, form.requestId, form.turnId, form.itemId, {
        submissionId: id,
        answers: Object.fromEntries(selected.map(([key, value]) => [key, { answers: [value] }])),
      });
      if (result.mode !== form.mode) throw new Error("The selected question changed.");
      setOutcome({ state: result.status, original: form.status });
      for (const key of locked) store.finish(key, result.status === "answered");
      if (result.status === "pending") submission.current = undefined;
      onUpdate();
    } catch (failure) {
      for (const key of locked) store.finish(key, false);
      setNotice(
        includesSecret
          ? "ReviewQ could not confirm the native answer. Check the request or open it in the terminal."
          : describeError(failure),
      );
      setOutcome({ state: "uncertain", original: form.status });
      onUpdate();
    } finally {
      busy.current = false;
    }
  }

  useEffect(() => {
    const form = element.current;
    const clear = () => clearSecrets(form);
    window.addEventListener("pagehide", clear);
    return () => {
      clear();
      window.removeEventListener("pagehide", clear);
    };
  }, [open]);

  return (
    <form
      className="question-form"
      ref={element}
      aria-label="Answer Codex question"
      onKeyDown={(event) => {
        if (event.key === "Escape" && !minimized) {
          event.preventDefault();
          event.stopPropagation();
          setMinimized(true);
          heading.current?.focus({ preventScroll: true });
        } else if (
          (event.target instanceof HTMLTextAreaElement ||
            (event.target instanceof HTMLInputElement && event.target.type === "password")) &&
          submits(event.nativeEvent)
        ) {
          event.preventDefault();
          void submit();
        }
      }}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <div className="question-heading">
        <button
          ref={heading}
          type="button"
          className="quiet-button"
          aria-label={minimized ? title : "Minimize question"}
          aria-controls={bodyId}
          aria-expanded={!minimized}
          onClick={() => setMinimized(!minimized)}
        >
          <Icon name="chevron" size={12} />
          <span>{title}</span>
        </button>
        <small title={destination}>{destination}</small>
      </div>
      <div id={bodyId} hidden={minimized}>
        {notice && <Notice>{notice}</Notice>}
        {reviewable.length > 0 && (
          <details className="answered-question">
            <summary>{reviewable.length === 1 ? "View question" : "View questions"}</summary>
            {reviewable.map((question) => (
              <section key={question.id} className="stack">
                <strong>{question.header}</strong>
                <p>{question.question}</p>
                {question.options.length > 0 && (
                  <ul>
                    {question.options.map((option) => (
                      <li key={option.label}>
                        {option.label}
                        {option.description && ` · ${option.description}`}
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            ))}
          </details>
        )}
        {status === "pending" && !open && !readOnly && (
          <div className="actions">
            <p className="meta">
              {form.mode === "async" && includesSecret
                ? "Enter this asynchronous secret answer in the native session."
                : "Use the native controls for this request."}
            </p>
            <button type="button" onClick={onTerminal}>
              Open in terminal
            </button>
          </div>
        )}
        {form.questions.map((question) =>
          question.isSecret ? (
            open && <SecretQuestion key={question.id} question={question} />
          ) : (
            <QuestionField
              key={question.id}
              question={question}
              store={store}
              draftKey={answerKey(prefix, question.id)}
              draftName={draftName}
              open={open}
              readOnly={readOnly}
            />
          ),
        )}
        {(open || readOnly) && (
          <div className="question-actions">
            {open ? (
              <small>Enter in an answer sends · Esc minimizes</small>
            ) : (
              <button type="button" onClick={onTerminal}>
                Open in terminal
              </button>
            )}
            <button type="submit" className="primary" disabled={!open}>
              {form.questions.length === 1 ? "Send answer" : "Send answers"}
            </button>
          </div>
        )}
        {status === "unavailable" && (
          <div className="actions">
            <p className="meta">This native request is no longer available in ReviewQ.</p>
            <button type="button" onClick={onTerminal}>
              Open in terminal
            </button>
          </div>
        )}
        {status === "uncertain" && (
          <div className="actions">
            <button type="button" onClick={onUpdate}>
              Check native question
            </button>
            <button type="button" onClick={onTerminal}>
              Open in terminal
            </button>
          </div>
        )}
        {form.questions.length === 0 && <DetachedAnswers store={store} prefix={prefix} />}
      </div>
    </form>
  );
}

function DetachedAnswers({
  store,
  prefix,
}: {
  readonly store: DraftStore;
  readonly prefix: readonly string[];
}) {
  const snapshot = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const questionPrefix = JSON.stringify(["question", ...prefix]).slice(0, -1) + ",";
  return snapshot.entries
    .filter((draft) => draft.key.startsWith(questionPrefix))
    .map((draft) => (
      <RetainedAnswer
        key={draft.key}
        draft={draft.text}
        onDiscard={() => store.set(draft.key, draft.label, "")}
      />
    ));
}

function RetainedAnswer({
  draft,
  onDiscard,
}: {
  readonly draft: string;
  readonly onDiscard: () => void;
}) {
  const [error, setError] = useState<string>();
  return (
    <div className="actions">
      <p className="meta">This unsent answer is retained.</p>
      <button
        type="button"
        onClick={() => {
          setError(undefined);
          void navigator.clipboard
            .writeText(questionValue(draft))
            .catch((failure: unknown) => setError(describeError(failure)));
        }}
      >
        Copy
      </button>
      <button
        type="button"
        onClick={() => {
          if (window.confirm("Discard this unsent answer?")) onDiscard();
        }}
      >
        Discard
      </button>
      {error && <Notice>{error}</Notice>}
    </div>
  );
}

function SecretQuestion({ question }: { readonly question: Question }) {
  return (
    <fieldset data-question={question.id} className="stack">
      <legend>{question.header}</legend>
      <p>{question.question}</p>
      <label className="field">
        {question.options.length > 0 && !question.isOther ? "Private selection" : "Secret answer"}
        {question.options.length > 0 && !question.isOther ? (
          <select name={secretName(question.id)} autoComplete="off" defaultValue="">
            <option value="">Choose an answer</option>
            {question.options.map((option) => (
              <option key={option.label} value={option.label}>
                {option.label}
              </option>
            ))}
          </select>
        ) : (
          <input
            type="password"
            name={secretName(question.id)}
            autoComplete="off"
            maxLength={8192}
          />
        )}
      </label>
    </fieldset>
  );
}

function secretInput(form: HTMLFormElement | null, questionId: string) {
  const element = form?.elements.namedItem(secretName(questionId));
  return element instanceof HTMLInputElement || element instanceof HTMLSelectElement
    ? element
    : undefined;
}

function clearSecrets(form: HTMLFormElement | null) {
  for (const element of form?.querySelectorAll<HTMLInputElement | HTMLSelectElement>(
    '[name^="native-private-"]',
  ) ?? []) {
    element.value = "";
  }
}

function secretName(questionId: string) {
  return `native-private-${questionId}`;
}

function QuestionField({
  question,
  store,
  draftKey,
  draftName,
  open,
  readOnly,
}: {
  readonly question: Question;
  readonly store: DraftStore;
  readonly draftKey: string;
  readonly draftName: string;
  readonly open: boolean;
  readonly readOnly: boolean;
}) {
  const draft = useDraft(store, draftKey);
  const [limited, setLimited] = useState(false);
  const answer = draft.startsWith("text:") ? draft.slice(5) : "";
  const other = useRef<HTMLTextAreaElement>(null);
  const [focused, setFocused] = useState(0);
  const legend = useId();
  const options = question.options;
  const choices =
    question.isOther && options.length > 0
      ? [...options, { label: "Other", description: "", other: true }]
      : options;
  function edit(value: string) {
    setLimited(!store.set(draftKey, draftName, value));
  }
  if (!open && !readOnly && !draft) return null;
  return (
    <fieldset data-question={question.id} className="stack">
      <legend id={legend}>{question.header || question.question}</legend>
      {question.header && <p>{question.question}</p>}
      {(open || readOnly) && (
        <>
          {choices.length > 0 && (
            <div
              className="question-choices"
              role="radiogroup"
              aria-labelledby={legend}
              onKeyDown={(event) => {
                const up = ["ArrowUp", "ArrowLeft"].includes(event.key);
                const down = ["ArrowDown", "ArrowRight"].includes(event.key);
                if (!up && !down && event.key !== "Home" && event.key !== "End") return;
                const buttons = [
                  ...event.currentTarget.querySelectorAll<HTMLButtonElement>("button"),
                ];
                const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
                if (at < 0) return;
                event.preventDefault();
                const next =
                  event.key === "Home"
                    ? 0
                    : event.key === "End"
                      ? buttons.length - 1
                      : (at + (up ? -1 : 1) + buttons.length) % buttons.length;
                buttons[next]?.focus({ preventScroll: true });
              }}
            >
              {choices.map((option, index) => {
                const custom = "other" in option && option.other === true;
                const checked = custom
                  ? draft.startsWith("text:")
                  : draft === `option:${option.label}`;
                return (
                  <button
                    type="button"
                    role="radio"
                    className="question-option"
                    key={custom ? "free-answer" : `option:${option.label}`}
                    aria-checked={checked}
                    disabled={!open}
                    tabIndex={focused === index ? 0 : -1}
                    onFocus={() => setFocused(index)}
                    onClick={() => {
                      if (!custom) edit(`option:${option.label}`);
                      else {
                        if (!checked) edit("text:");
                        requestAnimationFrame(() => other.current?.focus({ preventScroll: true }));
                      }
                    }}
                  >
                    <span className="choice-mark" aria-hidden="true">
                      {checked ? "●" : "○"}
                    </span>
                    <span>
                      <strong>{option.label}</strong>
                      {option.description && <small>{option.description}</small>}
                    </span>
                  </button>
                );
              })}
            </div>
          )}
          {open && (question.options.length === 0 || draft.startsWith("text:")) && (
            <label className="field">
              {question.options.length ? "Other answer" : "Your answer"}
              <textarea
                ref={other}
                value={answer}
                onChange={(event) => edit(`text:${event.currentTarget.value}`)}
                maxLength={12_000}
                rows={2}
              />
            </label>
          )}
          {open && limited && (
            <Notice>Draft storage full. Free space before adding more text.</Notice>
          )}
        </>
      )}
      {!open && draft && <RetainedAnswer draft={draft} onDiscard={() => edit("")} />}
    </fieldset>
  );
}

function answerKey(prefix: readonly string[], questionId: string) {
  return JSON.stringify(["question", ...prefix, questionId]);
}

function questionValue(draft: string) {
  return draft.slice(draft.indexOf(":") + 1);
}
