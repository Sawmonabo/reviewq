import type { QuestionAddress, SessionAddress } from "../../../../api/history";

type DraftAddress = Omit<QuestionAddress, "turnId"> & { readonly turnId: string | null };

export function questionKey(address: QuestionAddress) {
  return JSON.stringify([address.requestId, address.turnId, address.itemId]);
}

export function questionDraftKey(
  target: SessionAddress,
  form: QuestionAddress,
  questionId: string,
) {
  return JSON.stringify(["question", ...draftScope(target, form), questionId]);
}

export function questionDraftPrefix(target: SessionAddress, form: DraftAddress) {
  return JSON.stringify(["question", ...draftScope(target, form)]).slice(0, -1) + ",";
}

function draftScope(target: SessionAddress, { requestId, turnId, itemId }: DraftAddress) {
  const opening = [target.runId, target.sessionKey, requestId, itemId];
  return turnId === null ? opening : [...opening, turnId];
}
