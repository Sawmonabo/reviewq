type Priority = "foreground" | "ahead";

type Pending = {
  readonly start: () => void;
  readonly priority: Priority;
};

export type PatchReadQueue = ReturnType<typeof createPatchReadQueue>;

export function createPatchReadQueue() {
  const pending: Pending[] = [];
  let active = 0;

  function advance() {
    while (active < 3 && pending.length) {
      const first = pending.findIndex((task) => task.priority === "foreground");
      if (first < 0 && active >= 2) return;
      pending.splice(Math.max(0, first), 1)[0]!.start();
    }
  }

  return function read<T>(signal: AbortSignal, priority: Priority, load: () => Promise<T>) {
    return new Promise<T>((resolve, reject) => {
      signal.throwIfAborted();
      const task = {
        priority,
        start() {
          signal.removeEventListener("abort", cancel);
          active += 1;
          void Promise.resolve()
            .then(() => {
              signal.throwIfAborted();
              return load();
            })
            .then(resolve, reject)
            .finally(() => {
              active -= 1;
              advance();
            });
        },
      };
      function cancel() {
        const index = pending.indexOf(task);
        if (index >= 0) pending.splice(index, 1);
        reject(new DOMException("The patch read was canceled.", "AbortError"));
      }
      signal.addEventListener("abort", cancel, { once: true });
      pending.push(task);
      advance();
    });
  };
}
