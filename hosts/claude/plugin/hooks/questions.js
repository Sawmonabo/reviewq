import { invoke, waitForQuestion } from "./bridge.js";

function signature(questions) {
  if (!Array.isArray(questions)) return undefined;
  const encoded = JSON.stringify(questions, (_, value) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return value;
    return Object.fromEntries(Object.keys(value).sort().map(key => [key, value[key]]));
  });
  return encoded.length <= 42 * 1024 ? encoded : undefined;
}

export async function observeQuestion($, state, event, next, refresh) {
  const callId = event.tool_use_id;
  const prior = state.questionCalls.get(callId);
  if (prior) {
    prior.ambiguous = true;
    return next(event);
  }
  const question = { signature: signature(event.questions), address: undefined, waiting: false };
  state.questionCalls.set(callId, question);
  state.question = callId;
  refresh();
  let status = "closed";
  try {
    const turnId = state.activeTurn;
    if (turnId && question.signature && !event.answers && !event.annotations) {
      try {
        const messages = await $.session.messages({ last: 8 });
        const openings = messages.flatMap(message => message.role === "assistant" ?
          message.toolUses.filter(use => use.tool_use_id === callId && use.tool === "AskUserQuestion" &&
            signature(use.input?.questions) === question.signature).map(() => message.uuid) : []);
        if (state.activeTurn === turnId && openings.length === 1) {
          question.address = { generation: state.generation, turnId, assistantId: openings[0], callId };
        }
      } catch {
        // The native question still runs when its transcript is unavailable.
      }
    }
    const result = await next(event);
    if (!result.deny && !result.isError) {
      status = Object.keys(result.result?.answers ?? {}).length || result.result?.response ? "answered" : "unavailable";
    }
    return result;
  } finally {
    state.questionCalls.delete(callId);
    if (state.question === callId) state.question = state.questionCalls.keys().next().value;
    if (question.waiting) await invoke($, "question-finished", state.id, { address: question.address, status });
    refresh();
  }
}

export async function answerQuestion($, state, event, next, heartbeat, current) {
  const input = event.tool_input;
  if (event.tool_name !== "AskUserQuestion" || !state?.linked || event.session_id !== state.id ||
      event.agent_id || input?.answers || input?.annotations) {
    return next(event);
  }
  const encoded = signature(input?.questions);
  if (!encoded) return next(event);
  // The classic callback omits the tool-use ID. Bind it only to a unique open
  // tool call with this unchanged input and the exact native assistant opening.
  const matching = () => [...state.questionCalls.values()].filter(question => question.signature === encoded);
  const [question, ...otherCalls] = matching();
  if (!question?.address || question.ambiguous || otherCalls.length || question.waiting) return next(event);
  const active = () => current() && !question.ambiguous && !next.signal.aborted && state.activeTurn === question.address.turnId &&
    matching().length === 1 && state.questionCalls.get(question.address.callId) === question;
  question.waiting = true;
  try {
    await heartbeat();
    if (active()) {
      const answers = await waitForQuestion($, state.id, question.address);
      if (answers && active()) return { decision: { behavior: "allow", updatedInput: { ...input, answers } } };
    }
  } catch {
    // The terminal permission flow stays available through disconnect and timeout.
  }
  return next(event);
}
