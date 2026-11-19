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

const handoff = {
  type: "object",
  properties: {
    outcome: { enum: ["final", "unchanged", "needs_input"] },
    source: {
      type: "object",
      properties: { kind: { enum: ["file", "response"] }, path: { type: "string" }, sha256: digest },
      required: ["kind"],
      additionalProperties: false,
    },
    revision: {
      type: "object",
      properties: { sha: { anyOf: [sha, { type: "null" }] }, reason: { type: "string" } },
      required: ["sha"],
      additionalProperties: false,
    },
    verdict: { enum: ["approve", "request_changes", "blocked", "not_specified"] },
    reason: { type: "string", maxLength: 1024 },
    accepted: { type: "object" },
    amendsReport: { type: "object" },
    correctsReport: { type: "object" },
    assessments: { type: "array", maxItems: 10, items: { type: "object" } },
    findingChanges: { type: "array", maxItems: 50, items: { type: "object" } },
    sectionChanges: { type: "array", maxItems: 20, items: { type: "object" } },
  },
  required: ["outcome"],
  additionalProperties: false,
};

export async function registerWorkflowTools($) {
  await $.tool.register({
    name: "report_handoff",
    description: "Stage findings for this linked review lead. For final, supply an absolute Markdown file or the final native response, an exact reviewed SHA or an explicit unknown reason, and the verdict. For unchanged, supply the accepted receipt and source. For needs_input, supply the reason. The receipt is pending until ReviewQ verifies this native tool result.",
    inputSchema: { type: "object", properties: { handoff }, required: ["handoff"], additionalProperties: false },
  });
  await $.tool.register({
    name: "review_action",
    description: "Record an explicit user's authorization for the stated GitLab notes or approval, or reconcile the current authorization. The tool records and verifies evidence. Use GitLab through the usual native workflow only under the user's exact authorization.",
    inputSchema: { type: "object", properties: {
      operation: { enum: ["declare", "reconcile"] }, declaration,
    }, required: ["operation"], additionalProperties: false },
  });
}

export async function stageReport($, invoke, state, event) {
  const context = { turnId: state.activeTurn, callId: event.tool_use_id, handoff: event.handoff };
  let result = await invoke($, "report", state.id, context);
  if (result.error) result = await invoke($, "report-status", state.id, context);
  return stringifyResult(result);
}

export async function stageAction($, invoke, state, event) {
  const context = { callId: event.tool_use_id };
  const declare = event.operation === "declare";
  if (declare) {
    context.turnId = state.activeTurn;
    context.declaration = event.declaration;
  }
  const result = await invoke($, declare ? "action" : "action-status", state.id, context);
  return stringifyResult(result);
}

export async function reportTurn($, invoke, state, event) {
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
    return { result: result.error ?? "ReviewQ did not record this native tool call.", isError: true };
  }
  return { result: JSON.stringify(result.data) };
}
