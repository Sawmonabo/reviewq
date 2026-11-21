import type { SourcePage, SourceTarget } from "./source-page";

type ReadSource = (target: SourceTarget, signal: AbortSignal) => Promise<SourcePage>;
type Prepared = {
  readonly controller: AbortController;
  readonly page: Promise<SourcePage | undefined>;
};

export function createPatchLookahead() {
  const prepared = new Map<string, Prepared>();
  let latency = 45;
  let speed = 0;
  let previous = 0;

  function motion(delta: number) {
    const now = performance.now();
    const elapsed = now - previous;
    previous = now;
    const sample = delta / Math.max(12, Math.min(40, elapsed));
    speed =
      elapsed > 160 || Math.sign(sample) !== Math.sign(speed)
        ? sample
        : speed * 0.25 + sample * 0.75;
  }

  function distance(height: number) {
    const pixels = performance.now() - previous > 160 ? 0 : Math.abs(speed) * (latency + 32);
    return Math.min(height * 14, Math.max(height * 2, height + pixels));
  }

  async function measure(load: () => Promise<SourcePage>) {
    const started = performance.now();
    const page = await load();
    latency = latency * 0.45 + (performance.now() - started) * 0.55;
    return page;
  }

  function prepare(targets: readonly SourceTarget[], read: ReadSource) {
    const wanted = targets.slice(0, 2);
    const keys = new Set(wanted.map(sourceTargetKey));
    for (const [key, entry] of prepared) {
      if (keys.has(key)) continue;
      entry.controller.abort();
      prepared.delete(key);
    }
    for (const target of wanted) {
      const key = sourceTargetKey(target);
      if (prepared.has(key)) continue;
      const controller = new AbortController();
      const page = measure(() => read(target, controller.signal)).catch(() => undefined);
      prepared.set(key, { controller, page });
    }
  }

  async function take(target: SourceTarget, signal: AbortSignal) {
    const key = sourceTargetKey(target);
    const entry = prepared.get(key);
    if (!entry) return;
    prepared.delete(key);
    const cancel = () => entry.controller.abort();
    signal.addEventListener("abort", cancel, { once: true });
    try {
      signal.throwIfAborted();
      const page = await entry.page;
      signal.throwIfAborted();
      return page;
    } finally {
      signal.removeEventListener("abort", cancel);
    }
  }

  return {
    motion,
    distance,
    measure,
    prepare,
    take,
    clear() {
      for (const { controller } of prepared.values()) controller.abort();
      prepared.clear();
    },
  };
}

export function sourceTargetKey(target: SourceTarget) {
  if ("row" in target) return `row:${target.row}`;
  if ("before" in target) return `before:${target.before}`;
  if ("offset" in target) return `byte:${target.offset}`;
  return "end";
}
