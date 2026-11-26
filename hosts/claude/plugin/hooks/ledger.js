import { invoke } from "./bridge.js";
import { ledgerTools } from "./ledger-schema.js";
import { toolResult } from "./workflow.js";

// How much longer than a tool's deadline the plugin waits for the bridge, which spends that
// deadline on the service, so the bridge always answers first with ReviewQ's own message.
const BRIDGE_MARGIN_MS = 5000;
// The host's own fields on a tool-call event. An input key that collides with one of them is
// moved under `$shadowed`.
const SHADOWED = "$shadowed";
const HOST_FIELDS = new Set(["tool", "tool_use_id", "agentId", "requestMeta", "consent", SHADOWED]);

export const ledgerToolIds = ledgerTools.map(tool => `mcp__reviewq__${tool.name}`);

export async function registerLedgerTools($) {
  for (const { name, description, inputSchema } of ledgerTools) {
    await $.tool.register({ name, description, inputSchema });
  }
}

// One bridge spawn per call. The bridge makes the signed status request that proves the
// service, then posts the call; no heartbeat or transcript work. ReviewQ itself answers every
// call it can be reached for, including one from a conversation that is not linked and a
// closing call from a subagent. `state` is the session's state, absent before it is known.
export async function callLedger($, state, event) {
  const tool = ledgerTools[ledgerToolIds.indexOf(event.tool)];
  if (!tool) return { result: "ReviewQ does not serve this tool.", isError: true };
  const call = { tool: tool.name, callId: event.tool_use_id, input: toolInput(event) };
  if (state?.activeTurn) call.turnId = state.activeTurn;
  if (event.agentId) call.agentId = event.agentId;
  const id = state?.id ?? await $.session.id();
  return toolResult(await invoke($, "ledger", id, call, tool.deadlineMs + BRIDGE_MARGIN_MS));
}

// The event spreads the tool's arguments beside the host's fields. Drop only the host's and put
// back the arguments it shadowed, so the service's validator refuses a misspelled field by the
// model's own name instead of the plugin losing it silently.
function toolInput(event) {
  const input = Object.fromEntries(Object.entries(event).filter(([key]) => !HOST_FIELDS.has(key)));
  const shadowed = event[SHADOWED];
  return shadowed && typeof shadowed === "object" ? { ...input, ...shadowed } : input;
}
