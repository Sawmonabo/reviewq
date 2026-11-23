type QuestionDraft = {
  readonly choices: readonly string[];
  readonly other: string | null;
};

export function readQuestionDraft(draft: string): QuestionDraft {
  if (draft.startsWith("option:")) return { choices: [draft.slice(7)], other: null };
  if (draft.startsWith("text:")) return { choices: [], other: draft.slice(5) };
  if (draft.startsWith("choices:")) {
    const value: unknown = JSON.parse(draft.slice(8));
    if (
      value &&
      typeof value === "object" &&
      "choices" in value &&
      Array.isArray(value.choices) &&
      value.choices.every((choice: unknown) => typeof choice === "string") &&
      "other" in value &&
      (value.other === null || typeof value.other === "string")
    )
      return { choices: value.choices, other: value.other };
  }
  return { choices: [], other: null };
}

export function writeQuestionDraft(choices: readonly string[], other: string | null) {
  if (!choices.length) return other === null ? "" : `text:${other}`;
  if (choices.length === 1 && other === null) return `option:${choices[0]}`;
  return `choices:${JSON.stringify({ choices, other })}`;
}

export function questionDraftValues(draft: string) {
  const { choices, other } = readQuestionDraft(draft);
  return other === null ? choices : [...choices, other];
}
