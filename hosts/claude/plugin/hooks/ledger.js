import { invoke } from "./bridge.js";
import { ledgerTools } from "./ledger-schema.js";
import { toolResult } from "./workflow.js";

// The bridge's limit plus 5 seconds, so the bridge always answers first and its own timeout
// message reaches the model: 30 seconds for a record call, 55 for the closing call, which
// composes and publishes the report.
const RECORD_TIMEOUT_MS = 35000;
const FINISH_TIMEOUT_MS = 60000;
// The host's own fields on a tool-call event. An input key that collides with one of them is
// moved under `$shadowed`.
const SHADOWED = "$shadowed";
const HOST_FIELDS = new Set(["tool", "tool_use_id", "agentId", "requestMeta", "consent", SHADOWED]);
const UNLINKED = "This session is not linked to a ReviewQ review. Ask the user to run /reviewq <merge request or ticket> to link it, then call the tool again.";
const LEAD_CLOSES = "Only the lead agent closes the review. File your findings and notes with the record tools and report back to the lead.";

export const ledgerToolIds = ledgerTools.map(tool => `mcp__reviewq__${tool.name}`);

export async function registerLedgerTools($) {
  for (const { name, description, inputSchema } of ledgerTools) {
    await $.tool.register({ name, description, inputSchema });
  }
}

// One bridge spawn per call: no heartbeat, status request or transcript work.
export async function callLedger($, state, event) {
  const tool = ledgerTools[ledgerToolIds.indexOf(event.tool)];
  if (!tool) return { result: "ReviewQ does not serve this tool.", isError: true };
  if (!state?.linked) return { result: UNLINKED, isError: true };
  if (tool.leadOnly && event.agentId) return { result: LEAD_CLOSES, isError: true };
  const call = { tool: tool.name, callId: event.tool_use_id, input: toolInput(event) };
  if (state.activeTurn) call.turnId = state.activeTurn;
  if (event.agentId) call.agentId = event.agentId;
  const timeoutMs = tool.name === "finish_review" ? FINISH_TIMEOUT_MS : RECORD_TIMEOUT_MS;
  return toolResult(await invoke($, "ledger", state.id, call, timeoutMs));
}

// The event spreads the tool's arguments beside the host's fields. Drop only the host's and put
// back the arguments it shadowed, so the service's validator refuses a misspelled field by the
// model's own name instead of the plugin losing it silently.
function toolInput(event) {
  const input = Object.fromEntries(Object.entries(event).filter(([key]) => !HOST_FIELDS.has(key)));
  const shadowed = event[SHADOWED];
  return shadowed && typeof shadowed === "object" ? { ...input, ...shadowed } : input;
}
