import { useId, useRef, useState } from "react";
import type { Question } from "../../../../api/generated/contracts/common";
import { Notice } from "../../../../components/PanelState";
import { useDraft } from "../../../../hooks/useDraft";
import type { DraftStore } from "../../../../lib/drafts";

export function QuestionField({
  question,
  store,
  draftKey,
  draftName,
}: {
  readonly question: Question;
  readonly store: DraftStore;
  readonly draftKey: string;
  readonly draftName: string;
}) {
  const name = useId();
  const other = useRef<HTMLTextAreaElement>(null);
  const composition = useRef(false);
  const draft = useDraft(store, draftKey);
  const [limited, setLimited] = useState(false);
  const [composing, setComposing] = useState<string>();
  const freeText = draft.startsWith("text:");
  const options = question.options;
  function edit(value: string) {
    setLimited(!store.set(draftKey, draftName, value));
  }
  return (
    <fieldset className="question-field" data-question={question.id}>
      <legend>
        <QuestionWording question={question} />
      </legend>
      {options.length > 0 && (
        <div className="question-choices">
          {options.map((option, index) => (
            <label key={option.label} className="question-option">
              <input
                type="radio"
                name={name}
                checked={draft === `option:${option.label}`}
                aria-labelledby={`${name}-choice-${index}`}
                aria-describedby={option.description ? `${name}-description-${index}` : undefined}
                onChange={() => edit(`option:${option.label}`)}
              />
              <span>
                <strong id={`${name}-choice-${index}`}>{option.label}</strong>
                {option.description && (
                  <small id={`${name}-description-${index}`}>{option.description}</small>
                )}
              </span>
            </label>
          ))}
          {question.isOther && (
            <label className="question-option">
              <input
                type="radio"
                name={name}
                checked={freeText}
                onChange={() => {
                  edit("text:");
                  requestAnimationFrame(() => other.current?.focus());
                }}
              />
              Other
            </label>
          )}
        </div>
      )}
      {(!options.length || (question.isOther && freeText)) && (
        <label className="field">
          {options.length ? "Other answer" : "Your answer"}
          <textarea
            ref={other}
            value={composing ?? (freeText ? draft.slice(5) : "")}
            rows={2}
            maxLength={8192}
            onCompositionStart={(event) => {
              composition.current = true;
              setComposing(event.currentTarget.value);
            }}
            onCompositionEnd={(event) => {
              composition.current = false;
              setComposing(undefined);
              edit(`text:${event.currentTarget.value}`);
            }}
            onChange={(event) => {
              const next = event.currentTarget.value;
              if (composition.current) setComposing(next);
              else edit(next || options.length ? `text:${next}` : "");
            }}
          />
        </label>
      )}
      {limited && <Notice>Draft storage full. Free space before adding more text.</Notice>}
    </fieldset>
  );
}

export function SecretQuestionField({ question }: { readonly question: Question }) {
  const [other, setOther] = useState(false);
  const options = question.options;
  return (
    <fieldset className="question-field" data-question={question.id}>
      <legend>
        <QuestionWording question={question} />
      </legend>
      {options.length > 0 && (
        <label className="field">
          Private selection
          <select
            name={secretName(question.id, "choice")}
            data-private-answer
            autoComplete="off"
            defaultValue=""
            onChange={(event) => setOther(event.currentTarget.value === "other")}
          >
            <option value="">Choose an answer</option>
            {options.map((option, index) => (
              <option key={option.label} value={index}>
                {option.label}
                {option.description && ` · ${option.description}`}
              </option>
            ))}
            {question.isOther && <option value="other">Other</option>}
          </select>
        </label>
      )}
      {(!options.length || other) && (
        <label className="field">
          Secret answer
          <input
            type="password"
            name={secretName(question.id, "text")}
            data-private-answer
            autoComplete="off"
            maxLength={8192}
          />
        </label>
      )}
    </fieldset>
  );
}

export function QuestionWording({ question }: { readonly question: Question }) {
  return (
    <>
      {question.header && question.question && question.header !== question.question && (
        <small>{question.header}</small>
      )}
      {question.question || question.header}
    </>
  );
}

export function secretAnswer(form: HTMLFormElement, question: Question) {
  const choice = form.elements.namedItem(secretName(question.id, "choice"));
  if (choice instanceof HTMLSelectElement && choice.value !== "other")
    return choice.value ? (question.options[Number(choice.value)]?.label ?? "") : "";
  const input = form.elements.namedItem(secretName(question.id, "text"));
  return input instanceof HTMLInputElement ? input.value : "";
}

export function clearSecrets(form: HTMLFormElement | null) {
  for (const input of form?.querySelectorAll<HTMLInputElement | HTMLSelectElement>(
    "[data-private-answer]",
  ) ?? [])
    input.value = "";
}

function secretName(id: string, kind: "choice" | "text") {
  return `native-private-${kind}-${id}`;
}
