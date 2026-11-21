import { createContext } from "react";
import type { DrawingHint } from "../../api/generated/contracts/common";

const MAX_DRAWINGS = 4;
const MAX_BYTES = 192 * 1024;

type Artwork = {
  readonly svg: string;
  readonly id: string;
  readonly reusable?: string | undefined;
};

export class DiagramCache {
  private readonly ready = new Map<string, string>();
  private readonly pending = new Map<string, Promise<Artwork>>();
  private lifetime = new AbortController();

  constructor(readonly scope: string) {}

  async acquire(
    key: string,
    prepare: (signal: AbortSignal) => Promise<Artwork>,
  ): Promise<Artwork | string> {
    const existing = this.ready.get(key);
    if (existing !== undefined) {
      this.ready.delete(key);
      this.ready.set(key, existing);
      return existing;
    }
    const outstanding = this.pending.get(key);
    if (outstanding) return outstanding;
    const { signal } = this.lifetime;
    if (this.pending.size >= MAX_DRAWINGS) return prepare(signal);
    const promise = prepare(signal);
    this.pending.set(key, promise);
    try {
      const result = await promise;
      if (!signal.aborted && result.reusable) this.remember(key, result.reusable);
      return result;
    } finally {
      if (this.pending.get(key) === promise) this.pending.delete(key);
    }
  }

  clear() {
    this.lifetime.abort();
    this.lifetime = new AbortController();
    this.pending.clear();
    this.ready.clear();
  }

  private remember(key: string, svg: string) {
    const bytes = 2 * (key.length + svg.length);
    if (bytes > MAX_BYTES) return;
    let retained = 0;
    for (const [previous, drawing] of this.ready)
      retained += 2 * (previous.length + drawing.length);
    for (const [first, drawing] of this.ready) {
      if (this.ready.size < MAX_DRAWINGS && retained + bytes <= MAX_BYTES) break;
      retained -= 2 * (first.length + drawing.length);
      this.ready.delete(first);
    }
    this.ready.set(key, svg);
  }
}

export const DiagramCacheContext = createContext<DiagramCache | null>(null);
export const DiagramDisplayContext = createContext<readonly DrawingHint[] | undefined>(undefined);
