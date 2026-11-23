import { useEffect, useRef, useState } from "react";
import type { Question } from "../../../../api/generated/contracts/common";
import type { Form } from "../../../../api/generated/contracts/questions";
import { Notice } from "../../../../components/PanelState";
import { submits } from "../../../../lib/submit-key";
import {
  clearSecrets,
  QuestionField,
  QuestionOptions,
  QuestionWording,
  secretAnswer,
  SecretQuestionField,
} from "./QuestionField";
import { questionDraftValues } from "../../../../lib/question-draft";
import { questionDraftKey } from "./question-key";
import { canAnswer } from "./useQuestions";
import type { Questions } from "./useQuestions";

const MAX_ANSWER_BYTES = 8192;
const utf8 = new TextEncoder();

export function QuestionPrompt({
  form,
  questions,
  error,
  queue,
  onCancel,
}: {
  readonly form: Form;
  readonly questions: Questions;
  readonly error: string | undefined;
  readonly queue?: string;
  readonly onCancel?: () => void;
}) {
  const element = useRef<HTMLFormElement>(null);
  const [notice, setNotice] = useState<string>();
  const { target, store, draftName } = questions;
  const open = canAnswer(form);

  useEffect(() => {
    const current = element.current;
    const clear = () => clearSecrets(current);
    window.addEventListener("pagehide", clear);
    return () => {
      clear();
      window.removeEventListener("pagehide", clear);
    };
  }, [open]);

  function submit() {
    const current = element.current;
    if (!open || !current) return;
    const drafts = store.getSnapshot().entries;
    const answers = form.questions.map((question) => ({
      question,
      values: question.isSecret
        ? [secretAnswer(current, question)]
        : questionDraftValues(
            drafts.find(({ key }) => key === questionDraftKey(target, form, question.id))?.text ??
              "",
          ),
    }));
    const invalid = answers.find(
      ({ question, values }) =>
        !values.length ||
        (!question.multiSelect && values.length !== 1) ||
        values.some(
          (value) =>
            !value.trim() ||
            utf8.encode(value).length > MAX_ANSWER_BYTES ||
            /\p{Cc}/u.test(value.replace(/[\t\r\n]/g, "")) ||
            (!question.isOther &&
              question.options.length > 0 &&
              !question.options.some(({ label }) => label === value)),
        ),
    );
    if (invalid) {
      setNotice("Enter an answer of up to 8 KiB for each required question.");
      const field = current.querySelector(`[data-question="${CSS.escape(invalid.question.id)}"]`);
      (
        field?.querySelector<HTMLElement>("textarea, select, input[type=password]") ??
        field?.querySelector("input")
      )?.focus();
      return;
    }
    setNotice(undefined);
    clearSecrets(current);
    if (current.contains(document.activeElement)) current.focus({ preventScroll: true });
    void questions.respond(
      form,
      Object.fromEntries(answers.map(({ question, values }) => [question.id, values])),
    );
  }

  return (
    <form
      ref={element}
      className="question-form"
      aria-label="Answer agent question"
      aria-busy={form.status === "submitting" || undefined}
      tabIndex={-1}
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
      onChange={() => {
        if (notice) setNotice(undefined);
      }}
      onKeyDown={(event) => {
        if (
          (event.target instanceof HTMLTextAreaElement ||
            (event.target instanceof HTMLInputElement && event.target.type === "password")) &&
          submits(event.nativeEvent)
        ) {
          event.preventDefault();
          submit();
        }
      }}
    >
      <div className="question-form-body">
        {open ? (
          form.questions.map((question) =>
            question.isSecret ? (
              <SecretQuestionField key={question.id} question={question} />
            ) : (
              <QuestionField
                key={question.id}
                question={question}
                store={store}
                draftName={draftName}
                draftKey={questionDraftKey(target, form, question.id)}
              />
            ),
          )
        ) : (
          <QuestionContent items={form.questions} showChoices={form.status === "pending"} />
        )}
        {(notice || error) && <Notice>{notice ?? error}</Notice>}
      </div>
      <div className="question-actions">
        {queue && <small>{queue}</small>}
        <div className="question-actions-end">
          {open ? (
            <>
              {onCancel && (
                <button type="button" className="quiet-button" onClick={onCancel}>
                  Cancel
                </button>
              )}
              <button type="submit" className="primary">
                Answer
              </button>
            </>
          ) : (
            <QuestionStatus form={form} questions={questions} />
          )}
        </div>
      </div>
    </form>
  );
}

export function QuestionContent({
  items,
  showChoices = true,
}: {
  readonly items: readonly Question[];
  readonly showChoices?: boolean;
}) {
  return items.map((question) => (
    <div className="question-wording" key={question.id}>
      <p>
        <QuestionWording question={question} />
      </p>
      {showChoices && <QuestionOptions options={question.options} />}
    </div>
  ));
}

export function QuestionStatus({
  form,
  questions,
}: {
  readonly form: Form;
  readonly questions: Questions;
}) {
  if (form.status === "submitting") return <small role="status">Sending answer…</small>;
  return (
    <>
      {form.status === "uncertain" && (
        <>
          <small role="status">Answer delivery unconfirmed</small>
          <button type="button" className="quiet-button" onClick={questions.refresh}>
            Check request
          </button>
        </>
      )}
      <button type="button" className="quiet-button" onClick={questions.onTerminal}>
        Open in terminal
      </button>
    </>
  );
}
