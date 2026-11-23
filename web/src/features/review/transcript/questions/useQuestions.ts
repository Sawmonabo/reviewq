import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import type { Form } from "../../../../api/generated/contracts/questions";
import { answerQuestion, readQuestions } from "../../../../api/history";
import type { SessionAddress } from "../../../../api/history";
import { useResource } from "../../../../hooks/useResource";
import type { DraftStore } from "../../../../lib/drafts";
import { describeError } from "../../../../lib/presentation";
import { questionDraftKey, questionKey } from "./question-key";

type Attempt = {
  readonly status: Form["status"];
  readonly drafts: readonly string[];
  readonly error?: string;
};
type QuestionState = {
  readonly form: Form;
  readonly error: string | undefined;
};
const registries = new WeakMap<DraftStore, Map<string, Attempts>>();

export type Questions = ReturnType<typeof useQuestions>;

export function useQuestions(
  target: SessionAddress,
  store: DraftStore,
  draftName: string,
  onTerminal: () => void,
) {
  const { runId, sessionKey } = target;
  const [attempts] = useState(() => attemptsFor(store, target));
  const snapshot = useSyncExternalStore(attempts.subscribe, attempts.getSnapshot);
  const resource = useResource(
    `questions:${runId}:${sessionKey}`,
    (signal) => readQuestions(target, signal),
    questionResources(target),
  );
  const items = resource.data?.items;
  function stateFor(form: Form, failed = false): QuestionState {
    const attempt = snapshot.get(questionKey(form));
    const status =
      attempt &&
      needsResponse(form.status) &&
      !(form.status === "uncertain" && attempt.status === "submitting")
        ? attempt.status
        : form.status;
    return {
      form: {
        ...form,
        status,
        answerable: form.answerable && status === "pending" && !failed,
      },
      error: attempt?.error,
    };
  }
  const byKey = new Map(
    (items ?? []).map((form) => [questionKey(form), stateFor(form, Boolean(resource.error))]),
  );

  const reconcile = useCallback(
    (form: Form) => {
      const key = questionKey(form);
      const attempt = attempts.getSnapshot().get(key);
      if (!attempt || needsResponse(form.status)) return;
      finish(store, attempt, form.status === "answered" && !form.answeredElsewhere);
      attempts.set(key, undefined);
    },
    [attempts, store],
  );

  useEffect(() => {
    for (const item of items ?? []) reconcile(item);
  }, [items, reconcile]);

  async function respond(form: Form, values: Readonly<Record<string, readonly string[]>>) {
    const key = questionKey(form);
    const current = byKey.get(key)?.form ?? form;
    if (
      current.runId !== runId ||
      current.sessionKey !== sessionKey ||
      !canAnswer(current) ||
      attempts.getSnapshot().has(key)
    )
      return;
    const privateForm = current.questions.some((question) => question.isSecret);
    const locked = current.questions
      .filter((question) => !question.isSecret)
      .map((question) => questionDraftKey(target, current, question.id));
    for (const draft of locked) store.freeze(draft);
    const started: Attempt = { status: "submitting", drafts: locked };
    attempts.set(key, started);
    try {
      const result = await answerQuestion(target, current, {
        submissionId: crypto.randomUUID(),
        answers: Object.fromEntries(
          Object.entries(values).map(([id, answers]) => [id, { answers: [...answers] }]),
        ),
      });
      if (result.mode !== current.mode || result.blocking !== current.blocking)
        throw new Error("The native question changed.");
      if (attempts.getSnapshot().get(key) !== started) return;
      if (result.status === "pending" || !needsResponse(result.status)) {
        finish(store, started, result.status === "answered" && !result.answeredElsewhere);
        attempts.set(
          key,
          result.status === "pending" ? undefined : { status: result.status, drafts: [] },
        );
      } else attempts.set(key, { ...started, status: result.status });
    } catch (failure) {
      if (attempts.getSnapshot().get(key) === started)
        attempts.set(key, {
          ...started,
          status: "uncertain",
          error: privateForm
            ? "ReviewQ could not confirm the native answer."
            : describeError(failure),
        });
    } finally {
      resource.retry();
    }
  }

  const actions = [...byKey.values()];
  return {
    target,
    store,
    draftName,
    byKey,
    stateFor,
    reconcile,
    blocking: actions.filter(
      ({ form }) => form.blocking && form.mode === "native" && needsResponse(form.status),
    ),
    nonblocking: actions.flatMap(({ form }) =>
      !form.blocking && needsResponse(form.status) ? [form] : [],
    ),
    respond,
    refresh: resource.retry,
    onTerminal,
  };
}

export function questionResources({ runId, sessionKey }: SessionAddress) {
  return [
    { kind: "session", id: sessionKey },
    { kind: "history", id: sessionKey },
    { kind: "run", id: runId },
  ] as const;
}

export function needsResponse(status: Form["status"]) {
  return status === "pending" || status === "submitting" || status === "uncertain";
}

export function canAnswer(form: Form) {
  return (
    form.status === "pending" &&
    form.answerable &&
    form.questions.length > 0 &&
    ((form.mode === "native" && form.blocking) ||
      form.questions.every((question) => !question.isSecret))
  );
}

function finish(store: DraftStore, attempt: Attempt, accepted: boolean) {
  for (const key of attempt.drafts) store.finish(key, accepted);
}

function attemptsFor(store: DraftStore, { runId, sessionKey }: SessionAddress) {
  const scope = JSON.stringify([runId, sessionKey]);
  return (
    registries.get(store)?.get(scope) ??
    new Attempts((owner, size) => {
      const sessions = registries.get(store);
      if (!size) {
        if (sessions?.get(scope) === owner) sessions.delete(scope);
      } else if (sessions) sessions.set(scope, owner);
      else registries.set(store, new Map([[scope, owner]]));
    })
  );
}

class Attempts {
  #entries = new Map<string, Attempt>();
  #listeners = new Set<() => void>();
  readonly #retain: (owner: Attempts, size: number) => void;

  constructor(retain: (owner: Attempts, size: number) => void) {
    this.#retain = retain;
  }

  getSnapshot = () => this.#entries;

  subscribe = (listener: () => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
      if (!this.#listeners.size) {
        this.#entries = new Map(
          [...this.#entries].filter(([, attempt]) => needsResponse(attempt.status)),
        );
        this.#retain(this, this.#entries.size);
      }
    };
  };

  set(key: string, attempt: Attempt | undefined) {
    const next = new Map(this.#entries);
    if (attempt && (this.#listeners.size || needsResponse(attempt.status))) next.set(key, attempt);
    else next.delete(key);
    this.#entries = next;
    this.#retain(this, next.size);
    for (const listener of this.#listeners) listener();
  }
}
