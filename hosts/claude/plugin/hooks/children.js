import { invoke } from "./bridge.js";

const MAX_TRACKED = 2_048;
const MAX_INSPECTED = 64;
const MAX_ATTEMPTS = 4;
const FIRST_RETRY_MS = 5_000;
const MAX_RETRY_MS = 30_000;
const OVERFLOW_INTERVAL_MS = 60_000;
const encoder = new TextEncoder();

export async function reconcileChildren($, state) {
  const agents = await $.agent.list();
  const count = Math.min(agents.length, MAX_TRACKED);
  state.children ??= new Map();
  let index = count ? (state.childIndex ?? 0) % count : 0;
  let attempts = MAX_ATTEMPTS;
  let present;

  // Oversized rosters still progress without displacing acknowledged children.
  if (agents.length > count && Date.now() >= (state.overflowAt ?? 0)) {
    const index = (state.overflowIndex ?? 0) % (agents.length - count);
    state.overflowIndex = index + 1;
    state.overflowAt = Date.now() + OVERFLOW_INTERVAL_MS;
    const agent = agents[count + index];
    if (agent.id && agent.cwd) {
      const previous = state.overflowChild?.id === agent.id ? state.overflowChild : undefined;
      const metadata = childMetadata(agent);
      const signature = await fingerprint(metadata);
      if (needsAttempt(previous, signature)) {
        if (await $.session.id() !== state.id) return;
        state.overflowChild = await observe($, state, metadata, signature, previous);
        attempts -= 1;
      }
    }
  }

  for (let scanned = 0; scanned < Math.min(count, MAX_INSPECTED) && attempts; scanned += 1) {
    const agent = agents[index];
    index = (index + 1) % count;
    if (!agent.id || !agent.cwd) continue;
    const metadata = childMetadata(agent);
    const signature = await fingerprint(metadata);
    const previous = state.children.get(agent.id);
    if (!needsAttempt(previous, signature)) continue;
    if (!previous && state.children.size >= MAX_TRACKED) {
      present ??= new Set(agents.slice(0, count).map(agent => agent.id));
      for (const id of state.children.keys()) {
        if (!present.has(id)) state.children.delete(id);
      }
    }
    if (await $.session.id() !== state.id) return;
    state.children.set(agent.id, await observe($, state, metadata, signature, previous));
    attempts -= 1;
  }
  state.childIndex = index;
}

function needsAttempt(previous, signature) {
  return previous?.signature !== signature || (!previous.recorded && Date.now() >= previous.retryAt);
}

async function observe($, state, metadata, signature, previous) {
  const same = previous?.signature === signature;
  const sequence = same ? previous.sequence : ++state.childSequence;
  const result = await invoke($, "hook", state, {
    ...metadata,
    generation: state.generation,
    sequence,
  });
  const delay = same ? Math.min(previous.delay * 2, MAX_RETRY_MS) : FIRST_RETRY_MS;
  return {
    id: metadata.childId,
    signature,
    sequence,
    delay,
    retryAt: Date.now() + delay,
    recorded: !result.error,
  };
}

async function fingerprint(metadata) {
  const digest = await crypto.subtle.digest("SHA-256", encoder.encode(JSON.stringify(metadata)));
  return btoa(String.fromCharCode(...new Uint8Array(digest)));
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
