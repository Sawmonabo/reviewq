import { useEffect, useEffectEvent, useState } from "react";
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

  useEffect(() => {
    const controller = new AbortController();
    void readCurrent(controller.signal).then(
      ({ data, cursor }) => {
        if (controller.signal.aborted) return;
        setResult({ key, data, confirmedAt: Date.now() });
        observeSnapshot(cursor);
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
    return () => controller.abort();
  }, [key, epoch]);

  useEffect(() => {
    const entries = JSON.parse(keys) as ResourceKeyDto[];
    if (!entries.length) return;
    let timer: number | undefined;
    const unsubscribe = watchResources(entries, () => {
      if (timer === undefined) {
        timer = window.setTimeout(() => {
          timer = undefined;
          setEpoch((value) => value + 1);
        }, 85);
      }
    });
    return () => {
      unsubscribe();
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [keys]);

  const current = result?.key === key ? result : undefined;
  return { ...current, pending: current === undefined, retry: () => setEpoch((value) => value + 1) };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "ReviewQ could not load this information.";
}
