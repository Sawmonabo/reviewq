import type { QuestionAddress, SessionAddress } from "../../../../api/history";

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

export function questionDraftPrefix(target: SessionAddress, form: QuestionAddress) {
  return JSON.stringify(["question", ...draftScope(target, form)]).slice(0, -1) + ",";
}

function draftScope(target: SessionAddress, { requestId, turnId, itemId }: QuestionAddress) {
  return [target.runId, target.sessionKey, requestId, turnId, itemId];
}
