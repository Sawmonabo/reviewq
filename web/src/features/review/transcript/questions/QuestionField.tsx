import { useId, useRef, useState } from "react";
import type { Choice, Question } from "../../../../api/generated/contracts/common";
import { Notice } from "../../../../components/PanelState";
import { readQuestionDraft, writeQuestionDraft } from "../../../../lib/question-draft";
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
  const { choices, other: text } = readQuestionDraft(draft);
  const freeText = text !== null;
  const multiple = question.multiSelect === true;
  const options = question.options;
  function edit(selected: readonly string[], otherAnswer: string | null) {
    setLimited(!store.set(draftKey, draftName, writeQuestionDraft(selected, otherAnswer)));
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
                type={multiple ? "checkbox" : "radio"}
                name={name}
                checked={choices.includes(option.label)}
                aria-labelledby={`${name}-choice-${index}`}
                aria-describedby={
                  option.description || option.preview ? `${name}-detail-${index}` : undefined
                }
                onChange={(event) => {
                  const selected = multiple
                    ? options
                        .filter(({ label }) =>
                          label === option.label ? event.target.checked : choices.includes(label),
                        )
                        .map(({ label }) => label)
                    : [option.label];
                  edit(selected, multiple ? text : null);
                }}
              />
              <span>
                <strong id={`${name}-choice-${index}`}>{option.label}</strong>
                {(option.description || option.preview) && (
                  <span id={`${name}-detail-${index}`}>
                    {option.description && <small>{option.description}</small>}
                    {option.preview && choices.includes(option.label) && (
                      <span className="question-preview">{option.preview}</span>
                    )}
                  </span>
                )}
              </span>
            </label>
          ))}
          {question.isOther && (
            <label className="question-option">
              <input
                type={multiple ? "checkbox" : "radio"}
                name={name}
                checked={freeText}
                onChange={(event) => {
                  const enabled = event.target.checked;
                  edit(multiple ? choices : [], enabled ? "" : null);
                  if (enabled) requestAnimationFrame(() => other.current?.focus());
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
            value={composing ?? text ?? ""}
            rows={2}
            maxLength={8192}
            onCompositionStart={(event) => {
              composition.current = true;
              setComposing(event.currentTarget.value);
            }}
            onCompositionEnd={(event) => {
              composition.current = false;
              setComposing(undefined);
              edit(choices, event.currentTarget.value || (options.length ? "" : null));
            }}
            onChange={(event) => {
              const next = event.currentTarget.value;
              if (composition.current) setComposing(next);
              else edit(choices, next || (options.length ? "" : null));
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

export function QuestionOptions({
  options,
  selected,
}: {
  readonly options: readonly Choice[];
  readonly selected?: readonly string[] | undefined;
}) {
  const other = selected?.filter((answer) => !options.some(({ label }) => label === answer));
  return (
    <>
      {options.length > 0 && (
        <ul>
          {options.map((option) => (
            <li key={option.label}>
              {option.label}
              {selected?.includes(option.label) && (
                <small className="question-chosen">Selected</small>
              )}
              {option.description && ` · ${option.description}`}
              {option.preview && <span className="question-preview">{option.preview}</span>}
            </li>
          ))}
        </ul>
      )}
      {other?.map((answer) => (
        <p className="question-written-answer" key={answer}>
          <strong>Recorded answer</strong> {answer}
        </p>
      ))}
    </>
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
