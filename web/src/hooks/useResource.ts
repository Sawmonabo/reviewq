import { useEffect, useEffectEvent, useRef, useState } from "react";
import { observeSnapshot, watchResources } from "../api/events";
import type { ResourceKeyDto } from "../api/generated/contracts";

type Envelope<T> = { readonly data: T; readonly cursor: string };
type Result<T> = {
  readonly key: string;
  readonly data?: T;
  readonly error?: string;
  readonly confirmedAt?: number;
};

export function useResource<T>(
  key: string,
  read: (signal: AbortSignal) => Promise<Envelope<T>>,
  resources: readonly ResourceKeyDto[] = [],
) {
  const [result, setResult] = useState<Result<T>>();
  const [epoch, setEpoch] = useState(0);
  const readCurrent = useEffectEvent(read);
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
    const hide = () => controller.abort();
    window.addEventListener("pagehide", hide);
    void readCurrent(controller.signal).then(
      ({ data, cursor }) => {
        if (controller.signal.aborted) return;
        setResult({ key, data, confirmedAt: Date.now() });
        if (registered) registered.snapshot(cursor);
        else observeSnapshot(cursor);
      },
      (error: unknown) => {
        if (!controller.signal.aborted) {
          setResult((previous) => ({
            ...(previous?.key === key ? previous : {}),
            key,
            error: errorMessage(error),
          }));
        }
      },
    );
    return () => {
      controller.abort();
      window.removeEventListener("pagehide", hide);
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
