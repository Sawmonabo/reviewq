import { invoke } from "./bridge.js";
import { actionWaitMs } from "./ledger-schema.js";

const sha = { type: "string", pattern: "^(?:[a-f0-9]{40}|[a-f0-9]{64})$" };
const digest = { type: "string", pattern: "^[a-f0-9]{64}$" };
const id = { type: "string", minLength: 1, maxLength: 512 };

const declaration = {
  type: "object",
  properties: {
    intent: { enum: ["comments", "approval", "both", "cancel"] },
    reviewedSha: sha,
    comments: { type: "array", maxItems: 50, items: {
      type: "object",
      properties: { bodySha256: digest, discussionId: id },
      required: ["bodySha256"],
      additionalProperties: false,
    } },
  },
  required: ["intent", "reviewedSha"],
  additionalProperties: false,
};

export async function registerWorkflowTools($) {
  await $.tool.register({
    name: "review_action",
    description: "Record an explicit user's authorization for the stated GitLab notes or approval, or reconcile the current authorization. The tool records and verifies evidence. Use GitLab through the usual native workflow only under the user's exact authorization.",
    inputSchema: { type: "object", properties: {
      operation: { enum: ["declare", "reconcile"] }, declaration,
    }, required: ["operation"], additionalProperties: false },
  });
}

export async function stageAction($, state, event) {
  const context = { callId: event.tool_use_id };
  const declare = event.operation === "declare";
  // A declaration answers the latest prompt in the conversation. A prompt Claude Code took after
  // it, such as one the user typed during this turn, has not entered the conversation as a
  // prompt yet, so the declaration is refused, and so is one when no prompt was seen entering.
  if (declare && (state.promptSubmitted === undefined || state.submitted !== state.promptSubmitted)) {
    return refusal("The user has a newer instruction waiting. Nothing was recorded. Read it before you declare an action.");
  }
  if (declare) {
    context.turnId = state.turn.activeTurn;
    context.declaration = event.declaration;
  }
  const result = await invoke($, declare ? "action" : "action-status", state, context, actionWaitMs);
  return toolResult(result);
}

export async function reportTurn($, state, event) {
  await invoke($, "hook", state, {
    generation: state.generation, turnId: event.turnId, normal: event.reason === "answer",
  });
}

// A reply's `data` becomes the tool result. Its `error`, a sentence from `invoke` or the
// bridge's text for a refused ledger call, becomes a refusal.
export function toolResult(result) {
  if (!result.error && Object.hasOwn(result, "data")) return { result: JSON.stringify(result.data) };
  return refusal(result.error || "ReviewQ returned no result for this call.");
}

// Every refusal of a call to one of the plugin's own tools. The host gives `deny` to the model
// as the call's error result; an `isError` beside a hook's own `result` is ignored.
export function refusal(text) {
  return { deny: text };
}
