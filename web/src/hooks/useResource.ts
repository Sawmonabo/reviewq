import { useEffect, useEffectEvent, useRef, useState } from "react";
import { observeSnapshot, watchResources } from "../api/events";
import type { ResourceKeyDto } from "../api/generated/contracts";

type Envelope<T> = { readonly data: T; readonly cursor: string };
type Result<T> = {
  readonly key: string;
  readonly data?: T;
  readonly error?: string;
  readonly errorCause?: unknown;
  readonly confirmedAt?: number;
};

export function useResource<T>(
  key: string,
  read: (signal: AbortSignal) => Promise<Envelope<T>>,
  resources: readonly ResourceKeyDto[] = [],
  discardOnError?: (error: unknown) => boolean,
) {
  const [result, setResult] = useState<Result<T>>();
  const [epoch, setEpoch] = useState(0);
  const readCurrent = useEffectEvent(read);
  const discard = useEffectEvent((error: unknown) => discardOnError?.(error) ?? false);
  const keys = JSON.stringify(resources);
  const lease = useRef<ReturnType<typeof watchResources> | undefined>(undefined);

  useEffect(() => {
    const entries = JSON.parse(keys) as ResourceKeyDto[];
    if (!entries.length) return;
    let timer: number | undefined;
    const registered = watchResources(entries, () => {
      if (timer === undefined) {
        timer = window.setTimeout(() => {
          timer = undefined;
          setEpoch((value) => value + 1);
        }, 85);
      }
    });
    lease.current = registered;
    return () => {
      registered.close();
      if (lease.current === registered) lease.current = undefined;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [key, keys]);

  useEffect(() => {
    const controller = new AbortController();
    const registered = lease.current;
    let pending = true;
    function hide() {
      if (pending) controller.abort();
    }
    function restore(event: PageTransitionEvent) {
      if (event.persisted && controller.signal.aborted) setEpoch((value) => value + 1);
    }
    window.addEventListener("pagehide", hide);
    window.addEventListener("pageshow", restore);
    void readCurrent(controller.signal).then(
      ({ data, cursor }) => {
        pending = false;
        if (controller.signal.aborted) return;
        setResult({ key, data, confirmedAt: Date.now() });
        if (registered) registered.snapshot(cursor);
        else observeSnapshot(cursor);
      },
      (error: unknown) => {
        pending = false;
        if (!controller.signal.aborted) {
          const retain = !discard(error);
          setResult((previous) => ({
            ...(retain && previous?.key === key ? previous : {}),
            key,
            error: errorMessage(error),
            errorCause: error,
          }));
        }
      },
    );
    return () => {
      controller.abort();
      window.removeEventListener("pagehide", hide);
      window.removeEventListener("pageshow", restore);
    };
  }, [key, keys, epoch]);

  const current = result?.key === key ? result : undefined;
  return {
    ...current,
    pending: current === undefined,
    retry: () => setEpoch((value) => value + 1),
  };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "ReviewQ could not load this information.";
}
