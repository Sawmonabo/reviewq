import { useCallback, useEffect, useEffectEvent, useMemo, useState } from "react";
import { retryWorkspaceEvents, startWorkspaceEvents } from "../../api/events";
import type { EventConnectionStatus } from "../../api/events";
import type { WorkspaceIssue } from "../../hooks/useWorkspaceRecovery";

export type WorkspaceConnection = {
  readonly status: EventConnectionStatus;
  readonly issues: readonly WorkspaceIssue[];
  readonly retry: (() => void) | undefined;
};

const READ_RETRY_MS = 750;
const MAX_READ_RETRY_MS = 30_000;
const WARNING_DELAY_MS = 1_200;

export function useWorkspaceConnection() {
  const [status, setStatus] = useState<EventConnectionStatus>({ state: "ready" });
  const [entries, setEntries] = useState<ReadonlyMap<string, WorkspaceIssue>>(() => new Map());
  const [delayedWarning, setDelayedWarning] = useState(false);
  const register = useCallback((id: string, issue: WorkspaceIssue) => {
    setEntries((previous) => new Map(previous).set(id, issue));
    return () => {
      setEntries((previous) => {
        const next = new Map(previous);
        next.delete(id);
        return next;
      });
    };
  }, []);
  const pending = [...entries.values()];
  const issues = pending.filter((issue) => issue.kind !== "receipt");
  const interrupted = status.state === "reconnecting";
  const hasWarning = interrupted || issues.length > 0;
  const outage = interrupted || issues.some((issue) => issue.kind === "read");
  const recovery = useMemo(() => ({ register, outage }), [register, outage]);
  const canRetry = interrupted || issues.some((issue) => issue.retry);
  const hasRetryableReads = pending.some((issue) => issue.kind !== "sync" && issue.retry);
  const retryReads = useEffectEvent(() => {
    for (const issue of entries.values()) {
      if (issue.kind !== "sync") issue.retry?.();
    }
  });
  const retryReceipts = useEffectEvent(() => {
    for (const issue of entries.values()) {
      if (issue.kind === "receipt") issue.retry?.();
    }
  });
  if (!hasWarning && delayedWarning) setDelayedWarning(false);

  useEffect(
    () =>
      startWorkspaceEvents((next) => {
        setStatus(next);
        if (next.state === "ready") retryReceipts();
      }),
    [],
  );
  useEffect(() => {
    if (!hasWarning) return;
    const timer = window.setTimeout(() => setDelayedWarning(true), WARNING_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, [hasWarning]);
  useEffect(() => {
    if (!hasRetryableReads) return;
    let timer: number | undefined;
    let delay = READ_RETRY_MS;
    function schedule() {
      window.clearTimeout(timer);
      if (document.hidden) return;
      timer = window.setTimeout(() => {
        retryReads();
        delay = Math.min(MAX_READ_RETRY_MS, delay * 2);
        schedule();
      }, delay);
    }
    document.addEventListener("visibilitychange", schedule);
    schedule();
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("visibilitychange", schedule);
    };
  }, [hasRetryableReads]);

  const warning: WorkspaceConnection | undefined =
    hasWarning && delayedWarning
      ? {
          status,
          issues,
          retry: canRetry
            ? () => {
                retryWorkspaceEvents();
                for (const issue of issues) issue.retry?.();
              }
            : undefined,
        }
      : undefined;
  return { recovery, warning };
}
