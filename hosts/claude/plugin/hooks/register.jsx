import { deliver, finishTurn, observeFirstStep, observeInput, observeTurn, reviewqOrigin } from "./delivery.js";
import { registerWorkflowTools, reportTurn, stageAction, stageReport } from "./workflow.js";
import { childrenChanged, reconcileChildren } from "./children.js";

const HEARTBEAT_MS = 5000;
const OFFLINE_MS = 30000;
let session;
let timer;
let checking = false;
let recheck = false;

async function invoke($, action, id, value) {
  try {
    const home = await $.env.get("HOME");
    const binary = await $.env.get("REVIEWQ_NATIVE_BIN");
    if (!home?.startsWith("/") || (binary && !binary.startsWith("/"))) {
      return { error: "ReviewQ could not resolve the installed native helper." };
    }
    const argv = [binary || `${home}/.local/bin/reviewq`, "internal"];
    let stdin;
    if (action === "registration") {
      argv.push("claude-companion", id);
      if (value) argv.push("--target", value);
    } else {
      argv.push("claude-bridge", action, id);
      stdin = JSON.stringify(value);
    }
    const result = await $.process.run(argv, { stdin, timeoutMs: 5000, env: await $.process.hookEnv() });
    if (result.exitCode === 0 && !result.isStdoutTruncated) {
      return JSON.parse(result.stdout);
    }
    return { error: result.stderr.trim().slice(0, 1024) || "ReviewQ is unavailable." };
  } catch {
    return { error: "ReviewQ is unavailable. Check the installed service and plugin." };
  }
}

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
    if (result.linked && !state.linked) childrenChanged(state);
    state.linked = !!result.linked;
    await $.env.set("REVIEWQ_CLAUDE_LINKED", state.linked ? state.id : "");
    if (state.linked && !state.lastSeen) state.lastSeen = Date.now();
    $.ui.invalidate("ui.render");
  }
  return result;
}

async function heartbeat($, state, allowDelivery = true) {
  const result = await invoke($, "heartbeat", state.id, {
    generation: state.generation,
    sequence: ++state.sequence,
    phase: state.question ? "blocked" : state.working || state.activeTurn || state.pendingPrompt ? "working" : state.phase ?? null,
    hasQuestions: !!state.question,
    turnId: state.turnId ?? null,
    step: state.stepIndex ?? null,
    latestPromptId: state.lastPrompt ?? null,
    latestPromptTurnId: state.lastPromptTurn ?? null,
    latestPromptManual: !!state.lastPromptTurn && !!state.lastPromptManual,
    model: state.model ?? null,
    effort: state.effort ?? null,
  });
  if (state === session && await $.session.id() === state.id) {
    if (result.data?.protocolVersion === 1) {
      state.lastSeen = Date.now();
      state.label = result.data.label;
      if (allowDelivery) {
        await deliver($, invoke, state, result.data.offer);
        await reconcileChildren($, invoke, state);
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
      if (recheck || state !== previous) {
        recheck = false;
        await refresh($);
      }
      if (state === session && state.linked) await heartbeat($, state);
    } finally {
      checking = false;
      if (recheck) schedule($, 0);
      else if (session?.linked) schedule($, HEARTBEAT_MS);
    }
  });
}

export function register(on) {
  on("session.start", async ($, event, next) => {
    await registerWorkflowTools($);
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
    return { text: result.message ?? result.error, isTextDisplayOnly: true };
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
      if (session?.linked) {
        childrenChanged(session);
        schedule($);
      }
      return next(event);
    }
    const state = await current($);
    state.turnId = event.turnId;
    state.activeTurn = event.turnId;
    state.stepIndex = undefined;
    state.lastResponse = undefined;
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
    if (event.agentId && session?.linked) {
      childrenChanged(session);
      schedule($);
    }
    if (!event.agentId && session?.turnId === event.turnId) {
      const state = session;
      if (state.linked) {
        await heartbeat($, state, false);
        await reportTurn($, invoke, state, event);
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
    if (result.agentId && session?.linked) {
      childrenChanged(session);
      schedule($);
    }
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

  on("session.append", { door: "response" }, async ($, event, next) => {
    const result = await next(event);
    if (!event.agentId && result.uuid === event.uuid && session?.activeTurn
        && event.message.role === "assistant" && event.message.content.some(block => block.type === "text")) {
      session.lastResponse = result.uuid;
    }
    return result;
  });

  on("tool.call", { tool: "mcp__reviewq__report_handoff" }, async ($, event) => {
    const state = await current($);
    if (event.agentId || !state.linked || !state.activeTurn) {
      return { result: "Link the active lead with /reviewq before handing off a report.", isError: true };
    }
    await heartbeat($, state, false);
    return stageReport($, invoke, state, event);
  });

  on("tool.call", { tool: "mcp__reviewq__review_action" }, async ($, event) => {
    const state = await current($);
    if (event.agentId || !state.linked || !state.activeTurn) {
      return { result: "A ReviewQ action requires the linked lead and current user turn.", isError: true };
    }
    await heartbeat($, state, false);
    return stageAction($, invoke, state, event);
  });

  on("tool.call", { tool: "AskUserQuestion" }, async ($, event, next) => {
    if (event.agentId || !session?.linked) return next(event);
    const state = session;
    state.question = event.tool_use_id;
    schedule($);
    try {
      return await next(event);
    } finally {
      if (state.question === event.tool_use_id) state.question = undefined;
      if (state === session) schedule($);
    }
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
