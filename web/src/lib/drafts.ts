const MAX_DRAFTS = 128;
const MAX_BYTES = 8 * 1024 * 1024;
const ENTRY_BYTES = 256;

type Draft = { readonly key: string; readonly label: string; readonly text: string };
type Snapshot = { readonly entries: readonly Draft[]; readonly full: boolean };

export class DraftStore {
  #entries = new Map<string, Draft>();
  #pending = new Map<string, Draft>();
  #listeners = new Set<() => void>();
  #snapshot: Snapshot = { entries: [], full: false };

  getSnapshot = () => this.#snapshot;

  subscribe = (listener: () => void) => {
    this.#listeners.add(listener);
    return () => { this.#listeners.delete(listener); };
  };

  set(key: string, label: string, text: string): boolean {
    const current = this.#entries.get(key);
    if (current?.text === text && current.label === label) return true;
    const frozen = this.#pending.get(key);
    const alreadyShared = current === frozen;
    const charge = this.#budget();
    const next = text ? { key, label, text } : undefined;
    const subtract = current && !alreadyShared ? size(current) : 0;
    const count = charge.count - (current && !alreadyShared ? 1 : 0) + (next ? 1 : 0);
    if (next && (charge.bytes - subtract + size(next) > MAX_BYTES || count > MAX_DRAFTS)) {
      this.#publish(true);
      return false;
    }
    if (next) this.#entries.set(key, next);
    else this.#entries.delete(key);
    this.#publish(this.#snapshot.full && !released(current, frozen, next));
    return true;
  }

  freeze(key: string): Draft | undefined {
    const value = this.#entries.get(key);
    if (!value || this.#pending.has(key)) return;
    this.#pending.set(key, value);
    return value;
  }

  finish(key: string, accepted: boolean) {
    const captured = this.#pending.get(key);
    if (accepted && captured === this.#entries.get(key)) this.#entries.delete(key);
    this.#pending.delete(key);
    this.#publish(false);
  }

  #budget() {
    const values = new Set([...this.#entries.values(), ...this.#pending.values()]);
    let bytes = 0;
    for (const draft of values) bytes += size(draft);
    return { count: values.size, bytes };
  }

  #publish(full: boolean) {
    this.#snapshot = { entries: [...this.#entries.values()], full };
    for (const listener of this.#listeners) listener();
  }
}

function size(draft: Draft) {
  return ENTRY_BYTES + 2 * (draft.key.length + draft.label.length + draft.text.length);
}

function released(before: Draft | undefined, frozen: Draft | undefined, after: Draft | undefined) {
  return before !== undefined && before !== frozen && (!after || size(after) < size(before));
}
