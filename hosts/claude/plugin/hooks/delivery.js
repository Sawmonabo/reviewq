import { currentSettings } from "./settings.js";

const PLUGIN = "reviewq";

export function reviewqOrigin(origin) {
  return origin?.kind === "plugin" && origin.name === PLUGIN && origin.asUser === true;
}

function deliveryKey(state) {
  return `delivery.${state.id}`;
}

async function remember($, state, attempt) {
  if (attempt.outcome?.kind === "started") {
    await $.store.set(deliveryKey(state), { offer: attempt.offer, outcome: attempt.outcome });
  }
}

function canSend(state) {
  return state.linked && !state.working && state.phase === "idle" && !state.question
    && !state.draft && !state.pendingPrompt && !state.activeTurn;
}

async function postOutcome($, invoke, state, attempt) {
  const { offer, outcome } = attempt;
  if (!outcome || outcome === attempt.posted || await $.session.id() !== state.id) return;
  let accepted = outcome;
  let result = await invoke($, "hook", state.id, { offer, outcome });
  if (result.error && attempt.authorizationUnknown && !attempt.started
      && outcome.kind === "deferred" && await $.session.id() === state.id
      && state.delivery === attempt) {
    // A lost authorization reply can leave a sealed fence. This Mod has never
    // called prompt.submit for the offer, so it can safely settle either state.
    accepted = { kind: "dropped" };
    result = await invoke($, "hook", state.id, { offer, outcome: accepted });
  }
  if (!result.error && state.delivery === attempt) {
    attempt.posted = accepted;
    if (accepted.kind !== "uncertain") {
      state.delivery = undefined;
      if (accepted.kind === "started") await $.store.delete(deliveryKey(state));
    }
  }
}

export async function deliver($, invoke, state, offer) {
  if (!state.readRecovery) {
    const saved = await $.store.get(deliveryKey(state));
    if (saved?.offer && saved.outcome?.kind === "started" && !state.delivery) {
      state.delivery = saved;
    }
    state.readRecovery = true;
  }
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
    attempt.authorizationUnknown = true;
    attempt.outcome = { kind: "deferred" };
    await postOutcome($, invoke, state, attempt);
    return;
  }
  if (!canSend(state) || attempt.revision !== state.inputRevision || await $.session.id() !== state.id) {
    attempt.outcome = { kind: "dropped" };
    await postOutcome($, invoke, state, attempt);
    return;
  }

  if (authorization.data.settings) {
    const wanted = authorization.data.settings;
    const observed = await currentSettings($, state);
    if (observed?.model !== wanted.model || observed.effort !== wanted.effort
        || !canSend(state) || attempt.revision !== state.inputRevision || await $.session.id() !== state.id) {
      attempt.outcome = { kind: "dropped" };
      await postOutcome($, invoke, state, attempt);
      return;
    }
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
  await remember($, state, attempt);
}

export function finishTurn(state, turnId) {
  const attempt = state?.delivery;
  if (attempt?.started && attempt.turns.has(turnId) && !attempt.observed) {
    attempt.outcome = { kind: "uncertain" };
  }
}
