import { invoke } from "./bridge.js";
import { description, inputSchema } from "./report-handoff-schema.js";

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
    name: "report_handoff",
    description,
    inputSchema,
  });
  await $.tool.register({
    name: "review_action",
    description: "Record an explicit user's authorization for the stated GitLab notes or approval, or reconcile the current authorization. The tool records and verifies evidence. Use GitLab through the usual native workflow only under the user's exact authorization.",
    inputSchema: { type: "object", properties: {
      operation: { enum: ["declare", "reconcile"] }, declaration,
    }, required: ["operation"], additionalProperties: false },
  });
}

export async function stageReport($, state, event) {
  const context = { turnId: state.activeTurn, callId: event.tool_use_id, handoff: event.handoff };
  let result = await invoke($, "report", state.id, context);
  if (result.error && !result.correctionRequested) result = await invoke($, "report-status", state.id, context);
  return stringifyResult(result);
}

export async function stageAction($, state, event) {
  const context = { callId: event.tool_use_id };
  const declare = event.operation === "declare";
  if (declare) {
    context.turnId = state.activeTurn;
    context.declaration = event.declaration;
  }
  const result = await invoke($, declare ? "action" : "action-status", state.id, context);
  return stringifyResult(result);
}

export async function reportTurn($, state, event) {
  let response = null;
  if (event.reason === "answer" && state.lastResponse && event.answer) {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(event.answer));
    response = { entryId: state.lastResponse,
      sha256: Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, "0")).join(""),
    };
  }
  await invoke($, "hook", state.id, {
    generation: state.generation, turnId: event.turnId, normal: event.reason === "answer", response,
  });
}

function stringifyResult(result) {
  if (result.error || !Object.hasOwn(result, "data")) {
    if (Array.isArray(result.fieldErrors)) {
      return { result: JSON.stringify({ error: result.error, fieldErrors: result.fieldErrors }), isError: true };
    }
    return { result: result.error ?? "ReviewQ did not record this native tool call.", isError: true };
  }
  return { result: JSON.stringify(result.data) };
}
