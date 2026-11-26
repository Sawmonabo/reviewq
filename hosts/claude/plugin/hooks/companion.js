import { update } from "claude-code";

// The companion's identity and counters for the session, held by Claude Code in `$.state`,
// which outlives a reload of this module's code while module variables do not. A reloaded copy
// continues the same generation with the same proof, so ReviewQ keeps accepting it at once
// instead of refusing a new generation until the old one counts as gone. `$.state` never
// reaches the model, the transcript or a file; another plugin in the session may read it, as a
// plugin hooking `process.run` already reads the proof the helper takes on standard input.
const identity = { plugin: "reviewq", key: "companion" };
const progress = { plugin: "reviewq", key: "progress" };
const delivering = { plugin: "reviewq", key: "delivery" };

// This copy of the module. A reload's copy starts while the copy it replaces finishes its
// hooks, and both continue one generation, so ReviewQ offers both the same delivery.
const COPY = randomHex(8);

// Counters that only rise within a generation: ReviewQ refuses a report whose sequence does not.
const COUNTERS = ["sequence", "childSequence", "submitted"];

function randomHex(bytes) {
  return Array.from(crypto.getRandomValues(new Uint8Array(bytes)), byte => byte.toString(16).padStart(2, "0")).join("");
}

// The session's companion as a reload left it, or a new generation with a new proof.
export async function openCompanion($, id) {
  try {
    const [{ value: kept }, { value: counts }] = [await $.state.get(identity), await $.state.get(progress)];
    if (kept?.id === id) {
      const same = counts?.id === id && counts.generation === kept.generation ? counts : {};
      return {
        id, generation: kept.generation, proof: kept.proof,
        sequence: same.sequence ?? 0, childSequence: same.childSequence ?? 0,
        submitted: same.submitted ?? 0, promptSubmitted: same.promptSubmitted ?? undefined,
      };
    }
  } catch {
    // A new generation follows, as it would without the kept one.
  }
  const opened = { id, generation: randomHex(16), proof: randomHex(32), sequence: 0, childSequence: 0, submitted: 0 };
  try {
    await $.state.set(identity, { id, generation: opened.generation, proof: opened.proof });
  } catch {
    // This copy still runs; a reload then starts a new generation.
  }
  return opened;
}

// Stores the counters of `state` past any value another copy of this module stored for the same
// generation, raising `name` by one first when given, and answers the counter `name` now holds.
// The copy a reload replaces may still finish a report while the new one starts.
export async function keepCounters($, state, name) {
  if (name) state[name] = (state[name] ?? 0) + 1;
  try {
    const kept = await update($, progress, stored => {
      const base = stored?.id === state.id && stored.generation === state.generation ? stored : {};
      const value = { id: state.id, generation: state.generation };
      for (const key of COUNTERS) value[key] = Math.max(base[key] ?? 0, state[key] ?? 0);
      if (name && (base[name] ?? 0) >= state[name]) value[name] = base[name] + 1;
      const prompts = [base.promptSubmitted, state.promptSubmitted].filter(Number.isSafeInteger);
      if (prompts.length) value.promptSubmitted = Math.max(...prompts);
      return value;
    });
    for (const key of COUNTERS) state[key] = kept[key];
    if (Number.isSafeInteger(kept.promptSubmitted)) state.promptSubmitted = kept.promptSubmitted;
  } catch {
    // The value in memory stands; ReviewQ refuses a report that repeats a sequence.
  }
  return name ? state[name] : undefined;
}

// Whether this copy may work `offer`: no other copy of the module took it. Of two copies that
// both did, the one ReviewQ refused to authorize would settle the offer as never sent while the
// other sends it. A copy that dies holding an offer leaves it to ReviewQ, which settles it once
// its claim expires. When Claude Code cannot hold the mark, this copy goes ahead.
export async function claimDelivery($, state, offer) {
  const mark = `${offer.id}:${offer.fence}`;
  try {
    const kept = await update($, delivering, stored =>
      stored?.id === state.id && stored.offer === mark ? stored : { id: state.id, offer: mark, copy: COPY });
    return kept.copy === COPY;
  } catch {
    return true;
  }
}
