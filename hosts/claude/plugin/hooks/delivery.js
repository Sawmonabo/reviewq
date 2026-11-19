const PLUGIN = "reviewq";

export function reviewqOrigin(origin) {
  return origin?.kind === "plugin" && origin.name === PLUGIN && origin.asUser === true;
}

function canSend(state) {
  return state.linked && !state.working && state.phase === "idle" && !state.question
    && !state.draft && !state.pendingPrompt && !state.activeTurn;
}

async function postOutcome($, invoke, state, attempt) {
  const { offer, outcome } = attempt;
  if (!outcome || outcome === attempt.posted || await $.session.id() !== state.id) return;
  const result = await invoke($, "hook", state.id, { offer, outcome });
  if (!result.error && state.delivery === attempt) {
    attempt.posted = outcome;
    if (outcome.kind !== "uncertain") state.delivery = undefined;
  }
}

export async function deliver($, invoke, state, offer) {
  if (state.delivery) {
    await postOutcome($, invoke, state, state.delivery);
    return;
  }
  if (!offer || offer.generation !== state.generation) return;
  const attempt = {
    offer,
    revision: state.inputRevision,
    started: false,
    candidate: undefined,
    competing: false,
    turns: new Set(),
  };
  state.delivery = attempt;
  if (!canSend(state) || await $.session.id() !== state.id) {
    attempt.outcome = { kind: "deferred" };
    await postOutcome($, invoke, state, attempt);
    return;
  }
  const prior = await $.session.messages({ last: 1 });
  attempt.after = Array.isArray(prior) ? prior.at(-1)?.uuid : undefined;
  if (!attempt.after || attempt.revision !== state.inputRevision) {
    attempt.outcome = { kind: "deferred" };
    await postOutcome($, invoke, state, attempt);
    return;
  }
  const authorization = await invoke($, "delivery-check", state.id, { offer });
  if (!authorization.data) {
    // The response can be lost after the server sealed its single-use authorization.
    // Only the still-unissued server state accepts a deferral.
    attempt.outcome = { kind: "deferred" };
    await postOutcome($, invoke, state, attempt);
    return;
  }
  if (!canSend(state) || attempt.revision !== state.inputRevision || await $.session.id() !== state.id) {
    attempt.outcome = { kind: "dropped" };
    await postOutcome($, invoke, state, attempt);
    return;
  }

  attempt.started = true;
  attempt.context = authorization.data.reportContext;
  try {
    const accepted = await $.prompt.submit({ text: authorization.data.text, asUser: true });
    if (accepted.drop !== undefined && !attempt.candidate) {
      attempt.outcome = { kind: "dropped" };
    } else if (attempt.observed) {
      attempt.outcome = attempt.observed;
    }
  } catch {
    attempt.outcome = { kind: "uncertain" };
  }
  await postOutcome($, invoke, state, attempt);
}

export function observeInput(state, event, entryId) {
  const attempt = state?.delivery;
  if (!attempt?.started) return;
  if (!reviewqOrigin(event.origin) || attempt.candidate) {
    attempt.competing = true;
    return;
  }
  attempt.candidate = entryId;
}

export function observeTurn(state, turnId) {
  const attempt = state?.delivery;
  if (attempt?.started) attempt.turns.add(turnId);
}

export async function observeFirstStep($, state, event) {
  const attempt = state?.delivery;
  if (!attempt?.started || !attempt.candidate || attempt.competing ||
      attempt.turns.size !== 1 || !attempt.turns.has(event.turnId)) return;
  const following = await $.session.messages({ after: attempt.after });
  if (!Array.isArray(following)) return;
  const inputs = following.filter(row => row.role === "user");
  if (attempt.competing || attempt.turns.size !== 1 || inputs.length !== 1
      || inputs[0].uuid !== attempt.candidate) return;
  attempt.observed = {
    kind: "started", origin: PLUGIN, entryId: attempt.candidate, turnId: event.turnId,
  };
  attempt.outcome = attempt.observed;
}

export function finishTurn(state, turnId) {
  const attempt = state?.delivery;
  if (attempt?.started && attempt.turns.has(turnId) && !attempt.observed) {
    attempt.outcome = { kind: "uncertain" };
  }
}
