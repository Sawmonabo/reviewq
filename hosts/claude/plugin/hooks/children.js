import { invoke } from "./bridge.js";

const MAX_REMEMBERED = 128;
const MAX_INSPECTED = 64;
const MAX_ATTEMPTS = 4;
const FIRST_RETRY_MS = 5_000;
const MAX_RETRY_MS = 30_000;

export async function reconcileChildren($, state) {
  const agents = await $.agent.list();
  state.children ??= new Map();
  let index = agents.length ? (state.childIndex ?? 0) % agents.length : 0;
  let attempts = 0;
  const inspected = Math.min(agents.length, MAX_INSPECTED);
  for (let count = 0; count < inspected && attempts < MAX_ATTEMPTS; count += 1) {
    const agent = agents[index];
    index = (index + 1) % agents.length;
    if (!agent.id || !agent.cwd) continue;
    const metadata = childMetadata(agent);
    const signature = JSON.stringify(metadata);
    const previous = state.children.get(agent.id);
    const same = previous?.signature === signature;
    if (same && (previous.recorded || Date.now() < previous.retryAt)) continue;
    if (await $.session.id() !== state.id) return;
    const sequence = same ? previous.sequence : ++state.childSequence;
    attempts += 1;
    const result = await invoke($, "hook", state.id, {
      ...metadata,
      generation: state.generation,
      sequence,
    });
    const delay = same ? Math.min(previous.delay * 2, MAX_RETRY_MS) : FIRST_RETRY_MS;
    state.children.delete(agent.id);
    state.children.set(agent.id, {
      signature,
      sequence,
      delay,
      retryAt: Date.now() + delay,
      recorded: !result.error,
    });
    if (state.children.size > MAX_REMEMBERED) {
      state.children.delete(state.children.keys().next().value);
    }
  }
  state.childIndex = index;
}

function childMetadata(agent) {
  return {
    childId: agent.id,
    parentId: agent.parentId ?? null,
    childCwd: agent.cwd,
    status: agent.status,
    name: (agent.description || agent.name || agent.label || "").trim().slice(0, 128) || null,
    startedAtMs: Number.isSafeInteger(agent.startTime) ? agent.startTime : null,
    model: agent.lastRequest?.model ?? null,
    effort: agent.lastRequest?.effort == null ? null : String(agent.lastRequest.effort),
  };
}
