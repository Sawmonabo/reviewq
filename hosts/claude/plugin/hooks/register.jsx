import { invoke } from "./bridge.js";
import { deliver, finishTurn, observeFirstStep, observeInput, observeTurn, reviewqOrigin } from "./delivery.js";
import { registerWorkflowTools, reportTurn, stageAction } from "./workflow.js";
import { callLedger, ledgerToolIds, registerLedgerTools } from "./ledger.js";
import { reconcileChildren } from "./children.js";
import { currentSettings } from "./settings.js";
import { answerQuestion, observeQuestion } from "./questions.js";

const HEARTBEAT_MS = 5000;
const OFFLINE_MS = 30000;
const REGISTRATION_RETRY_MS = 400;
const REGISTRATION_DEADLINE_MS = 12000;
let session;
let timer;
let checking = false;
let recheck = false;

function reset() {
  timer?.cancel();
  timer = undefined;
  session = undefined;
}

async function current($) {
  const id = await $.session.id();
  if (session?.id !== id) {
    reset();
    const salt = crypto.getRandomValues(new Uint8Array(16));
    session = {
      id,
      generation: Array.from(salt, byte => byte.toString(16).padStart(2, "0")).join(""),
      sequence: 0,
      inputRevision: 0,
      childSequence: 0,
      questionCalls: new Map(),
      linked: false,
      lastSeen: 0,
      label: undefined,
    };
    $.ui.invalidate("ui.render");
  }
  return session;
}

async function refresh($, target) {
  const state = await current($);
  const result = await invoke($, "registration", state.id, target);
  if (state === session && await $.session.id() === state.id && !result.error) {
    state.linked = !!result.linked;
    if (result.pendingLaunch && !state.pendingUntil) {
      state.pendingUntil = Date.now() + REGISTRATION_DEADLINE_MS;
    }
    if (state.linked) state.pendingUntil = undefined;
    await $.env.set("REVIEWQ_CLAUDE_LINKED", state.linked ? state.id : "");
    if (state.linked && !state.lastSeen) state.lastSeen = Date.now();
    $.ui.invalidate("ui.render");
  }
  return result;
}

function pending(state) {
  return !!state?.pendingUntil && Date.now() < state.pendingUntil;
}

async function workflowLead($) {
  const state = await current($);
  if (!state.linked) await refresh($);
  for (let attempts = 0; state === session && !state.linked && pending(state) && attempts < 3; attempts += 1) {
    await $.clock.sleep(REGISTRATION_RETRY_MS);
    await refresh($);
  }
  return state;
}

async function heartbeat($, state, allowDelivery = true) {
  const settings = await currentSettings($, state);
  const result = await invoke($, "heartbeat", state.id, {
    generation: state.generation,
    sequence: ++state.sequence,
    phase: state.question ? "blocked" : state.working || state.activeTurn || state.pendingPrompt ? "working" : state.phase ?? null,
    hasQuestions: !!state.question,
    questionId: state.question ?? null,
    turnId: state.turnId ?? null,
    step: state.stepIndex ?? null,
    latestPromptId: state.lastPrompt ?? null,
    latestPromptTurnId: state.lastPromptTurn ?? null,
    latestPromptManual: !!state.lastPromptTurn && !!state.lastPromptManual,
    model: settings?.model ?? null,
    effort: settings?.effort ?? null,
  });
  if (state === session && await $.session.id() === state.id) {
    if (result.data?.protocolVersion === 1) {
      state.lastSeen = Date.now();
      state.label = result.data.label;
      if (allowDelivery) {
        await deliver($, state, result.data.offer);
        await reconcileChildren($, state);
      }
    }
    $.ui.invalidate("ui.render");
  }
}

function schedule($, delay = 0, registration = false) {
  recheck ||= registration;
  if (checking) return;
  timer?.cancel();
  timer = $.clock.after(delay, async () => {
    timer = undefined;
    checking = true;
    try {
      const previous = session;
      const state = await current($);
      if (recheck || state !== previous || pending(state)) {
        recheck = false;
        await refresh($);
      }
      if (state === session && state.linked) await heartbeat($, state);
    } finally {
      checking = false;
      if (recheck) schedule($, 0);
      else if (session?.linked) schedule($, HEARTBEAT_MS);
      else if (pending(session)) schedule($, REGISTRATION_RETRY_MS);
    }
  });
}

export function register(on) {
  on("session.start", async ($, event, next) => {
    await registerWorkflowTools($);
    await registerLedgerTools($);
    await $.command.register({
      name: "reviewq",
      description: "Show ReviewQ status or link this session to a merge request",
      argumentHint: "[merge request or ticket]",
    });
    schedule($, 0, true);
    return next(event);
  });

  on("command.run", { command: "reviewq" }, async ($, event) => {
    const result = await refresh($, (event.args ?? "").trim());
    if (session?.linked) schedule($);
    return result.rule ? { text: `${result.message}\n\n${result.rule}` } : { text: result.message ?? result.error, isTextDisplayOnly: true };
  });

  on("prompt.submit", async ($, event, next) => {
    const state = await current($);
    state.pendingPrompt = true;
    const owned = state.delivery?.started && reviewqOrigin(event.origin);
    if (!owned) state.inputRevision += 1;
    try {
      const context = owned ? state.delivery.context : undefined;
      return await next(context ? { ...event, context: [...(event.context ?? []), context] } : event);
    } finally {
      state.pendingPrompt = false;
      if (state === session && (state.linked || event.origin?.kind === "composer" || event.origin?.kind === "bridge")) {
        schedule($, 0, !state.linked);
      }
    }
  });

  on("turn.start", async ($, event, next) => {
    if (event.agentId) {
      if (session?.linked) schedule($);
      return next(event);
    }
    const state = await current($);
    state.turnId = event.turnId;
    state.activeTurn = event.turnId;
    state.stepIndex = undefined;
    state.phase = "working";
    observeTurn(state, event.turnId);
    if (state.linked) schedule($);
    return next(event);
  });

  on("turn.step", async function* ($, event, next) {
    if (!event.agentId && session?.turnId === event.turnId) {
      session.stepIndex = event.index;
      session.model = event.model;
      session.effort = typeof event.effort === "string" ? event.effort : undefined;
      if (event.index === 0) {
        const state = session;
        const prompt = state.lastPrompt;
        const native = await $.session.messages({ last: 1 });
        if (session === state && state.lastPrompt === prompt
            && Array.isArray(native) && native.at(-1)?.uuid === prompt && native.at(-1)?.role === "user") {
          state.lastPromptTurn = event.turnId;
        }
        if (state.delivery) await observeFirstStep($, state, event);
        if (state.linked) schedule($);
      }
    }
    return yield* next(event);
  });

  on("turn.complete", async ($, event, next) => {
    const result = await next(event);
    if (event.agentId && session?.linked) schedule($);
    if (!event.agentId && session?.turnId === event.turnId) {
      const state = session;
      if (state.linked) {
        await heartbeat($, state, false);
        await reportTurn($, state, event);
      }
      state.activeTurn = undefined;
      session.phase = event.reason === "error" ? "failed" : "idle";
      finishTurn(session, event.turnId);
      session.question = undefined;
      if (session.linked) schedule($);
    }
    return result;
  });

  on("agent.spawn", async ($, event, next) => {
    const result = await next(event);
    if (result.agentId && session?.linked) schedule($);
    return result;
  });

  on("session.append", { door: "prompt" }, async ($, event, next) => {
    const result = await next(event);
    if (!event.agentId && result.uuid === event.uuid && session?.linked
      && !event.message.isMeta && event.message.role === "user") {
      session.lastPrompt = result.uuid;
      session.lastPromptTurn = undefined;
      session.lastPromptManual = event.origin?.kind === "composer" || event.origin?.kind === "bridge";
      session.inputRevision += 1;
      observeInput(session, event, result.uuid);
    }
    return result;
  });

  on("tool.call", { tool: "mcp__reviewq__review_action" }, async ($, event) => {
    const state = event.agentId ? undefined : await workflowLead($);
    if (!state?.linked || !state.activeTurn) {
      return { result: "A ReviewQ action requires the linked lead and current user turn.", isError: true };
    }
    await heartbeat($, state, false);
    return stageAction($, state, event);
  });

  for (const tool of ledgerToolIds) {
    on("tool.call", { tool }, async ($, event) => callLedger($, event.agentId ? session : await workflowLead($), event));
  }

  on("tool.call", { tool: "AskUserQuestion" }, async ($, event, next) => {
    if (event.agentId || !session?.linked) return next(event);
    const state = session;
    return observeQuestion($, state, event, next, () => { if (state === session) schedule($); });
  });

  on("classic.PermissionRequest", { tool_name: "AskUserQuestion" }, async ($, event, next) => {
    const state = session;
    return answerQuestion($, state, event, next, () => heartbeat($, state, false), () => state === session);
  });

  on("session.end", async ($, event, next) => {
    const state = session;
    reset();
    recheck = false;
    if (state?.linked) {
      await $.env.set("REVIEWQ_CLAUDE_LINKED", "");
      await invoke($, "hook", state.id, { classic: { kind: "disconnected", generation: state.generation } });
    }
    return next(event);
  });

  on("ui.render", { component: "PromptHint" }, ($, event, next) => {
    if (event.surface !== "terminal" || !session?.linked) return next(event);
    session.working = event.props.isWorking;
    session.draft = event.props.isDraft;
    if (!session.working && !session.phase) session.phase = "idle";
    const offline = Date.now() - session.lastSeen >= OFFLINE_MS;
    const label = offline ? "Offline" : session.label;
    const suffix = label ? `ReviewQ · ${label}` : "ReviewQ";
    return next({ ...event, props: { ...event.props,
      tail: event.props.tail ? `${event.props.tail} · ${suffix}` : suffix,
    } });
  });
}
