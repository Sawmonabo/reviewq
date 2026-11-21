type Update = (visible: boolean) => void;

export function createThreadViewport() {
  const slots = new Map<HTMLElement, Set<Update>>();
  const nearby = new Set<HTMLElement>();
  let held = new Set<HTMLElement>();
  let observer: IntersectionObserver | undefined;
  let signals: AbortController | undefined;

  function publish(slot: HTMLElement) {
    const visible = nearby.has(slot) || held.has(slot);
    for (const update of slots.get(slot) ?? []) update(visible);
  }

  function selectionChanged() {
    const selection = getSelection();
    const endpoints = selection?.isCollapsed ? [] : [selection?.anchorNode, selection?.focusNode];
    const next = new Set<HTMLElement>();
    for (const node of [...endpoints, document.activeElement]) {
      const element = node instanceof HTMLElement ? node : node?.parentElement;
      const slot = element?.closest<HTMLElement>(".patch-thread-slot");
      if (slot && slots.has(slot)) next.add(slot);
    }
    const previous = held;
    held = next;
    for (const slot of new Set([...previous, ...next])) publish(slot);
  }

  return function observe(slot: HTMLElement, update: Update) {
    if (!observer) {
      const root = slot.closest(".drawer-body");
      observer = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            const target = entry.target as HTMLElement;
            if (entry.isIntersecting) nearby.add(target);
            else nearby.delete(target);
            publish(target);
          }
        },
        { root, rootMargin: "480px 0px" },
      );
      signals = new AbortController();
      for (const event of ["focusin", "focusout", "selectionchange"])
        document.addEventListener(event, selectionChanged, { signal: signals.signal });
    }
    const known = slots.get(slot);
    const updates = known ?? new Set();
    updates.add(update);
    slots.set(slot, updates);
    if (!known) observer.observe(slot);
    else publish(slot);
    return () => {
      updates.delete(update);
      if (updates.size) return;
      slots.delete(slot);
      held.delete(slot);
      nearby.delete(slot);
      observer?.unobserve(slot);
      if (slots.size) return;
      observer?.disconnect();
      observer = undefined;
      signals?.abort();
    };
  };
}
