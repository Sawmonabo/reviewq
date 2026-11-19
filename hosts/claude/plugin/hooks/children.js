const MAX_PENDING = 64;
const RETRY_MS = 30000;

export function childrenChanged(state) {
  state.inspectChildren = true;
}

export async function reconcileChildren($, invoke, state) {
  if (state.inspectChildren) {
    state.inspectChildren = false;
    const agents = await $.agent.list();
    state.children ??= new Map();
    const observed = agents.slice(0, MAX_PENDING);
    const activeIds = new Set(observed.map(agent => agent.id));
    for (const id of state.children.keys()) {
      if (!activeIds.has(id)) state.children.delete(id);
    }
    for (const agent of observed) {
      if (!agent.id || !agent.cwd) continue;
      const working = agent.status === "running" || agent.status === "pending";
      const previous = state.children.get(agent.id);
      if (!previous || previous.working !== working) {
        state.children.set(agent.id, {
          childId: agent.id,
          parentId: agent.parentId ?? null,
          childCwd: agent.cwd,
          working,
          generation: state.generation,
          sequence: ++state.childSequence,
          expires: Date.now() + RETRY_MS,
        });
      }
    }
  }
  let budget = 4;
  for (const child of state.children?.values() ?? []) {
    if (child.recorded || Date.now() >= child.expires) continue;
    if (budget-- === 0 || await $.session.id() !== state.id) break;
    const { expires, recorded, ...payload } = child;
    const result = await invoke($, "hook", state.id, payload);
    if (!result.error) child.recorded = true;
  }
}
