import { useEffect, useRef, useState } from "react";
import type { Form as NativeForm, Question } from "../../../api/generated/contracts/questions";
import { answerQuestion, readQuestions } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Notice } from "../../../components/PanelState";
import { useDraft } from "../../../hooks/useDraft";
import { useResource } from "../../../hooks/useResource";
import type { DraftStore } from "../../../lib/drafts";
import { describeError, readable } from "../../../lib/presentation";
import { submits } from "../../../lib/submit-key";

export function NativeQuestions({
  target,
  store,
  reviewTitle,
  onTerminal,
}: {
  readonly target: SessionAddress;
  readonly store: DraftStore;
  readonly reviewTitle: string;
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
  onTerminal,
  onUpdate,
}: {
  readonly form: NativeForm;
  readonly target: SessionAddress;
  readonly store: DraftStore;
  readonly label: string;
  readonly onTerminal: () => void;
  readonly onUpdate: () => void;
}) {
  const [outcome, setOutcome] = useState<{
    state: NativeForm["status"];
    original: NativeForm["status"];
  }>();
  const [notice, setNotice] = useState<string>();
  const submission = useRef<string | undefined>(undefined);
  const busy = useRef(false);
  const element = useRef<HTMLFormElement>(null);
  const status = outcome?.original === form.status ? outcome.state : form.status;
  const includesSecret = form.questions.some((question) => question.isSecret);
  const supported = form.questions.length > 0 && (!includesSecret || form.blocking);
  const open = status === "pending" && supported;
  const draftName = `Question · ${label}`;
  const prefix = [target.runId, target.sessionKey, form.requestId, form.turnId, form.itemId];

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
      className="surface-card stack question-form"
      ref={element}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <div className="spread">
        <h3>{form.blocking ? "Native input required" : "Native question"}</h3>
        <span className="badge rose">{readable(status)}</span>
      </div>
      {notice && <Notice>{notice}</Notice>}
      {!supported && (
        <div className="actions">
          <p className="meta">
            {includesSecret
              ? "Enter this asynchronous secret answer in the native session."
              : "Use the native controls for this request."}
          </p>
          <button type="button" onClick={onTerminal}>
            Open in terminal
          </button>
        </div>
      )}
      {supported &&
        form.questions.map((question) =>
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
            />
          ),
        )}
      {open && (
        <button type="submit" className="primary">
          Send answers
        </button>
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
    </form>
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
}: {
  readonly question: Question;
  readonly store: DraftStore;
  readonly draftKey: string;
  readonly draftName: string;
  readonly open: boolean;
}) {
  const draft = useDraft(store, draftKey);
  const [limited, setLimited] = useState(false);
  const answer = draft.startsWith("text:") ? draft.slice(5) : "";
  const other = useRef<HTMLTextAreaElement>(null);
  function edit(value: string) {
    setLimited(!store.set(draftKey, draftName, value));
  }
  if (!open && !draft) return null;
  return (
    <fieldset
      data-question={question.id}
      className="stack"
      onKeyDown={(event) => {
        if (submits(event.nativeEvent)) {
          event.preventDefault();
          event.currentTarget.closest("form")?.requestSubmit();
        }
      }}
    >
      <legend>{question.header}</legend>
      <p>{question.question}</p>
      {open ? (
        <>
          {question.options.map((option) => (
            <label className="question-option" key={option.label}>
              <input
                type="radio"
                name={draftKey}
                checked={draft === `option:${option.label}`}
                onChange={() => edit(`option:${option.label}`)}
              />
              <span>
                <strong>{option.label}</strong>
                {option.description && <small>{option.description}</small>}
              </span>
            </label>
          ))}
          {question.options.length > 0 && question.isOther && (
            <label>
              <input
                type="radio"
                name={draftKey}
                checked={draft.startsWith("text:")}
                onChange={() => {
                  edit("text:");
                  requestAnimationFrame(() => other.current?.focus());
                }}
              />{" "}
              Other
            </label>
          )}
          {(question.options.length === 0 || draft.startsWith("text:")) && (
            <label className="field">
              Your answer
              <textarea
                ref={other}
                value={answer}
                onChange={(event) => edit(`text:${event.currentTarget.value}`)}
                maxLength={12_000}
                rows={2}
              />
            </label>
          )}
          {limited && <Notice>Draft storage full. Free space before adding more text.</Notice>}
        </>
      ) : (
        <div className="actions">
          <p className="meta">This unsent answer is retained.</p>
          <button
            type="button"
            onClick={() => {
              void navigator.clipboard
                .writeText(questionValue(draft))
                .catch((error: unknown) => setLimited(Boolean(error)));
            }}
          >
            Copy
          </button>
          <button
            type="button"
            onClick={() => {
              if (window.confirm("Discard this unsent answer?")) edit("");
            }}
          >
            Discard
          </button>
        </div>
      )}
    </fieldset>
  );
}

function answerKey(prefix: readonly string[], questionId: string) {
  return JSON.stringify(["question", ...prefix, questionId]);
}

function questionValue(draft: string) {
  return draft.slice(draft.indexOf(":") + 1);
}
