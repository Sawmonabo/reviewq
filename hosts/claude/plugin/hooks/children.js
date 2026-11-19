import { invoke } from "./bridge.js";

const MAX_PENDING = 64;
const RETRY_MS = 30_000;

export async function reconcileChildren($, state) {
  const agents = await $.agent.list();
  const now = Date.now();
  state.children ??= new Map();
  const observed = agents.slice(0, MAX_PENDING).filter(agent => agent.id && agent.cwd);
  const activeIds = new Set(observed.map(agent => agent.id));
  for (const [id, child] of state.children) {
    if (!activeIds.has(id) && (child.recorded || now >= child.expires)) state.children.delete(id);
  }
  for (const agent of observed) {
    const metadata = {
      childId: agent.id,
      parentId: agent.parentId ?? null,
      childCwd: agent.cwd,
      status: agent.status,
      name: (agent.description || agent.name || agent.label || "").trim().slice(0, 128) || null,
      startedAtMs: Number.isSafeInteger(agent.startTime) ? agent.startTime : null,
      model: agent.lastRequest?.model ?? null,
      effort: agent.lastRequest?.effort == null ? null : String(agent.lastRequest.effort),
    };
    const signature = JSON.stringify(metadata);
    const previous = state.children.get(agent.id);
    if (!previous || previous.signature !== signature) {
      if (!previous && state.children.size >= MAX_PENDING) continue;
      state.children.set(agent.id, {
        metadata,
        signature,
        sequence: ++state.childSequence,
        expires: now + RETRY_MS,
      });
    }
  }
  let budget = 4;
  for (const child of state.children.values()) {
    if (child.recorded || now >= child.expires) continue;
    const parent = state.children.get(child.metadata.parentId);
    if (parent && !parent.recorded) continue;
    if (budget-- === 0 || await $.session.id() !== state.id) break;
    const result = await invoke($, "hook", state.id, {
      ...child.metadata,
      generation: state.generation,
      sequence: child.sequence,
    });
    if (!result.error) child.recorded = true;
  }
}
