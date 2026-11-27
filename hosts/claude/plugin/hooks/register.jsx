import { invoke } from "./bridge.js";
import { deliver, finishTurn, observeFirstStep, observeInput, observeTurn, reviewqOrigin } from "./delivery.js";
import { refusal, registerWorkflowTools, reportTurn, stageAction } from "./workflow.js";
import { callLedger, ledgerToolIds, registerLedgerTools } from "./ledger.js";
import { reconcileChildren } from "./children.js";
import { currentSettings } from "./settings.js";
import { observeQuestion } from "./questions.js";
import { keepCounters, keepTurn, newest, openCompanion, phaseOf, readTurn } from "./companion.js";

const HEARTBEAT_MS = 5000;
const OFFLINE_MS = 30000;
const REGISTRATION_RETRY_MS = 400;
const REGISTRATION_DEADLINE_MS = 12000;
let session;
let opening;
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
    // One read of the kept companion per session, however many hooks ask at once.
    if (opening?.id !== id) opening = { id, companion: openCompanion($, id) };
    const companion = await opening.companion;
    if (session?.id !== id) {
      reset();
      session = {
        // `proof` proves this module's requests to ReviewQ, which keeps only its hash; see `invoke`.
        ...companion,
        inputRevision: 0,
        questionCalls: new Map(),
        linked: false,
        lastSeen: 0,
        label: undefined,
      };
      $.ui.invalidate("ui.render");
    }
  }
  return session;
}

async function refresh($, target) {
  const state = await current($);
  const result = await invoke($, "registration", state, target);
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
  const turn = await readTurn($, state);
  const settings = await currentSettings($, state);
  const result = await invoke($, "heartbeat", state, {
    generation: state.generation,
    sequence: await keepCounters($, state, "sequence"),
    phase: phaseOf(state),
    hasQuestions: !!turn.question,
    questionId: turn.question ?? null,
    turnId: turn.turnId ?? null,
    step: turn.step ?? null,
    latestPromptId: turn.lastPrompt ?? null,
    latestPromptTurnId: turn.lastPromptTurn ?? null,
    latestPromptManual: !!turn.lastPromptTurn && !!turn.lastPromptManual,
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

// A turn this copy restored from the kept value and has heard nothing of: the copy that saw it
// start may have gone before it ended, leaving it running there. Claude Code's prompt says
// whether a turn runs, and only a hook that draws it hears that, so the kept turn ends from a
// timer, while the prompt still says idle. A step or the end of the turn reaching this copy
// shows it hears the turn's end itself.
function settleRestored($, state) {
  const turnId = state.restoredTurn;
  $.clock.after(0, async () => {
    if (state !== session || state.restoredTurn !== turnId || state.working !== false) return;
    state.restoredTurn = undefined;
    await keepTurn($, state, kept => kept.activeTurn === turnId ? { activeTurn: null, phase: "idle", question: null } : {});
    if (state.linked) schedule($);
  });
}

function hear(event) {
  if (!event.agentId && session?.restoredTurn === event.turnId) session.restoredTurn = undefined;
}

function schedule($, delay = 0, registration = false) {
  recheck ||= registration;
  if (checking) return;
  timer?.cancel();
  timer = $.clock.after(delay, async () => {
    timer = undefined;
    checking = true;
    let owner = true;
    try {
      const previous = session;
      const state = await current($);
      // A reload's copy took over: this one stops its loop.
      owner = await newest($);
      if (!owner) return;
      if (recheck || state !== previous || pending(state)) {
        recheck = false;
        await refresh($);
      }
      if (state === session && state.linked) await heartbeat($, state);
    } finally {
      checking = false;
      if (!owner) recheck = false;
      else if (recheck) schedule($, 0);
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
    // Every new input the person sees as typed, mid-turn and queued included; see `stageAction`.
    // A slash command, a prompt the person does not see as typed, and Claude Code's own notice
    // that a background task finished, which carries no such mark, are none: only a visible
    // prompt's row catches up below.
    if (!event.command && !event.isMeta && event.origin?.kind !== "task-notification") {
      await keepCounters($, state, "submitted");
    }
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
    await keepTurn($, state, { turnId: event.turnId, activeTurn: event.turnId, step: null, phase: "working" });
    observeTurn(state, event.turnId);
    if (state.linked) schedule($);
    return next(event);
  });

  on("turn.step", async function* ($, event, next) {
    hear(event);
    const state = event.agentId ? undefined : await current($);
    const effort = typeof event.effort === "string" ? event.effort : null;
    // One write before each model request; a step of another turn changes nothing.
    let ours = false;
    const turn = state && await keepTurn($, state, base =>
      (ours = base.turnId === event.turnId) ? { step: event.index, model: event.model, effort } : {});
    if (ours) {
      const prompt = turn.lastPrompt;
      if (event.index === 0) {
        const native = await $.session.messages({ last: 1 });
        if (session === state && prompt
            && Array.isArray(native) && native.at(-1)?.uuid === prompt && native.at(-1)?.role === "user") {
          await keepTurn($, state, kept => kept.lastPrompt === prompt ? { lastPromptTurn: event.turnId } : {});
        }
        if (state.delivery) await observeFirstStep($, state, event);
        if (state.linked) schedule($);
      }
    }
    return yield* next(event);
  });

  on("turn.complete", async ($, event, next) => {
    hear(event);
    const result = await next(event);
    if (event.agentId && session?.linked) schedule($);
    const state = event.agentId ? undefined : await current($);
    if (state && (await readTurn($, state)).turnId === event.turnId) {
      if (state.linked) {
        await heartbeat($, state, false);
        await reportTurn($, state, event);
      }
      // The turn ended, whatever a surface that has since detached last drew.
      state.working = false;
      await keepTurn($, state, { activeTurn: null, phase: event.reason === "error" ? "failed" : "idle", question: null });
      finishTurn(state, event.turnId);
      if (state.linked) schedule($);
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
      const state = session;
      state.inputRevision += 1;
      state.promptSubmitted = state.submitted;
      observeInput(state, event, result.uuid);
      await keepTurn($, state, {
        lastPrompt: result.uuid, lastPromptTurn: null,
        lastPromptManual: event.origin?.kind === "composer" || event.origin?.kind === "bridge",
      });
      await keepCounters($, state);
    }
    return result;
  });

  on("tool.call", { tool: "mcp__reviewq__review_action" }, async ($, event) => {
    const state = event.agentId ? undefined : await workflowLead($);
    if (!state?.linked || !(await readTurn($, state)).activeTurn) {
      return refusal("A ReviewQ action requires the linked lead and current user turn.");
    }
    await heartbeat($, state, false);
    return stageAction($, state, event);
  });

  for (const tool of ledgerToolIds) {
    on("tool.call", { tool }, async ($, event) => {
      const state = event.agentId ? session : await workflowLead($);
      if (state) await readTurn($, state);
      return callLedger($, state, event);
    });
  }

  // A main-loop call that asks the person reports at once, without holding the check. ReviewQ
  // stores a permission request that arrives while it cannot tell whether this companion still
  // runs, and binds it to the step the next report names, so that report lands while the dialog
  // is up. A subagent's permission request is not reported, so its calls are left alone. A check
  // without a tool call, such as a plugin's query, an `op` check or an MCP call that goes
  // straight to its server, shows no dialog.
  on("tool.check", async ($, event, next) => {
    const verdict = await next(event);
    const state = session;
    if (verdict?.decision === "ask" && event.tool_use_id && !event.agentId && state?.linked) {
      void heartbeat($, state, false).catch(() => {});
    }
    return verdict;
  });

  on("tool.call", { tool: "AskUserQuestion" }, async ($, event, next) => {
    if (event.agentId || !session?.linked) return next(event);
    const state = session;
    return observeQuestion($, state, event, next, {
      refresh: () => { if (state === session) schedule($); },
      heartbeat: () => heartbeat($, state, false),
      current: () => state === session,
    });
  });

  on("session.end", async ($, event, next) => {
    const state = session;
    reset();
    recheck = false;
    if (state?.linked) {
      await $.env.set("REVIEWQ_CLAUDE_LINKED", "");
      await invoke($, "hook", state, { classic: { kind: "disconnected", generation: state.generation } });
    }
    return next(event);
  });

  on("ui.render", { component: "PromptHint" }, ($, event, next) => {
    if (!session?.linked) return next(event);
    // Whether a turn runs is the session's on every surface that draws the hint; the typed
    // draft and the tail are the terminal's.
    session.working = event.props.isWorking;
    if (!session.working && session.restoredTurn && session.turn.activeTurn === session.restoredTurn) {
      settleRestored($, session);
    }
    if (event.surface !== "terminal") return next(event);
    session.draft = event.props.isDraft;
    const offline = Date.now() - session.lastSeen >= OFFLINE_MS;
    const label = offline ? "Offline" : session.label;
    const suffix = label ? `ReviewQ · ${label}` : "ReviewQ";
    return next({ ...event, props: { ...event.props,
      tail: event.props.tail ? `${event.props.tail} · ${suffix}` : suffix,
    } });
  });
}
