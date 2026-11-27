import { invoke, waitForQuestion } from "./bridge.js";
import { keepTurn } from "./companion.js";

function signature(questions) {
  if (!Array.isArray(questions)) return undefined;
  const encoded = JSON.stringify(questions, (_, value) => {
    if (!value || typeof value !== "object" || Array.isArray(value)) return value;
    return Object.fromEntries(Object.keys(value).sort().map(key => [key, value[key]]));
  });
  return encoded.length <= 42 * 1024 ? encoded : undefined;
}

// Watches the main loop's AskUserQuestion call. While Claude Code shows its dialog, ReviewQ's
// dashboard may answer the same question: the first answer stands, and an answer from the
// dashboard settles the call in the dialog's place, which closes it. This runs at `tool.call`,
// which reaches the module. A plugin seated above, as Claude Code's built-in security plugin can
// be, may pass classic events, the permission request among them, on past a user plugin's module.
export async function observeQuestion($, state, event, next, { refresh, heartbeat, current }) {
  const callId = event.tool_use_id;
  const prior = state.questionCalls.get(callId);
  if (prior) {
    prior.ambiguous = true;
    return next(event);
  }
  const question = { signature: signature(event.questions), address: undefined, waiting: false };
  state.questionCalls.set(callId, question);
  await keepTurn($, state, { question: callId });
  refresh();
  let status = "closed";
  let nativeAnswers;
  const stop = new AbortController();
  try {
    const turnId = state.turn.activeTurn;
    if (turnId && question.signature && !event.answers && !event.annotations) {
      try {
        const messages = await $.session.messages({ last: 8 });
        const openings = messages.flatMap(message => message.role === "assistant" ?
          message.toolUses.filter(use => use.tool_use_id === callId && use.tool === "AskUserQuestion" &&
            signature(use.input?.questions) === question.signature).map(() => message.uuid) : []);
        if (state.turn.activeTurn === turnId && openings.length === 1) {
          question.address = { generation: state.generation, turnId, assistantId: openings[0], callId };
        }
      } catch {
        // The native question still runs when its transcript is unavailable.
      }
    }
    const active = () => current() && !question.ambiguous && !next.signal.aborted && !stop.signal.aborted &&
      state.turn.activeTurn === question.address.turnId && state.questionCalls.get(callId) === question;
    const native = next(event).then(result => ({ result }));
    // A dialog closed by the dashboard's answer may still reject after the hook returned.
    native.catch(() => {});
    const answered = question.address ? remoteAnswer($, state, question, event.questions, active, heartbeat, stop.signal) : undefined;
    const first = answered ? await Promise.race([native, answered]) : await native;
    if (first.answers && active()) {
      status = "answered";
      nativeAnswers = first.answers;
      return { result: { questions: event.questions, answers: first.answers } };
    }
    stop.abort();
    const { result } = first.result ? first : await native;
    if (!result.deny && !result.isError) {
      nativeAnswers = result.result?.answers;
      status = Object.keys(nativeAnswers ?? {}).length || result.result?.response ? "answered" : "unavailable";
    }
    return result;
  } finally {
    stop.abort();
    state.questionCalls.delete(callId);
    // Only the question this call opened: a reload's copy may have opened another since.
    const following = state.questionCalls.keys().next().value ?? null;
    await keepTurn($, state, kept => kept.question === callId ? { question: following } : {});
    if (question.waiting) await invoke($, "question-finished", state, { address: question.address, status, nativeAnswers });
    refresh();
  }
}

// The dashboard's answers to `question`, or nothing once it cannot answer: the dialog then
// stays the only way to answer.
async function remoteAnswer($, state, question, questions, active, heartbeat, signal) {
  question.waiting = true;
  try {
    await heartbeat();
    if (!active()) return {};
    return { answers: await waitForQuestion($, state, question.address, questions, signal) };
  } catch {
    return {};
  }
}
