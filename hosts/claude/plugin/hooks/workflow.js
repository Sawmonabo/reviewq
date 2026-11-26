import { invoke } from "./bridge.js";

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
  if (declare) {
    context.turnId = state.activeTurn;
    context.declaration = event.declaration;
  }
  const result = await invoke($, declare ? "action" : "action-status", state.id, context);
  return toolResult(result);
}

export async function reportTurn($, state, event) {
  await invoke($, "hook", state.id, {
    generation: state.generation, turnId: event.turnId, normal: event.reason === "answer",
  });
}

// A reply's `data` becomes the tool result. `error` is a string from `invoke` or the service's
// error object; refused fields follow one per line so the model can correct the call. A refusal
// whose errors are all at the root names no field, so its messages stand alone, as the MCP
// server's tool error writes them.
export function toolResult(result) {
  if (!result.error && Object.hasOwn(result, "data")) return { result: JSON.stringify(result.data) };
  if (Array.isArray(result.fieldErrors)) {
    const lines = result.fieldErrors.map(({ path, message }) => path ? `${path}: ${message}` : message);
    if (!lines.length || result.fieldErrors.some(({ path }) => path)) lines.unshift("Invalid tool input. Correct these fields and call again.");
    return { result: lines.join("\n"), isError: true };
  }
  const error = typeof result.error === "string" ? result.error : result.error && JSON.stringify(result.error);
  return { result: error || "ReviewQ returned no result for this call.", isError: true };
}
