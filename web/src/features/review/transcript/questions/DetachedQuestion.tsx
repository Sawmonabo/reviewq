import { useId, useRef, useState } from "react";
import type { Form } from "../../../../api/generated/contracts/questions";
import { questionKey } from "./question-key";
import { QuestionPrompt } from "./QuestionPrompt";
import type { Questions } from "./useQuestions";

export function DetachedQuestion({
  form,
  questions,
}: {
  readonly form: Form;
  readonly questions: Questions;
}) {
  const [open, setOpen] = useState(false);
  const control = useRef<HTMLButtonElement>(null);
  const panel = useId();
  function close() {
    setOpen(false);
    control.current?.focus({ preventScroll: true });
  }
  return (
    <div
      className="detached-question"
      onKeyDown={(event) => {
        if (open && event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          close();
        }
      }}
    >
      <button
        type="button"
        ref={control}
        className="question-jump quiet-button"
        aria-expanded={open}
        aria-controls={panel}
        onClick={() => setOpen(!open)}
      >
        Open question
      </button>
      {open && (
        <div className="detached-question-body" id={panel}>
          <QuestionPrompt
            form={form}
            questions={questions}
            error={questions.byKey.get(questionKey(form))?.error}
            onCancel={close}
          />
        </div>
      )}
    </div>
  );
}
