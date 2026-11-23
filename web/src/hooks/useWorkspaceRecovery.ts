import { createContext, useContext, useEffect, useId, useRef } from "react";

export type WorkspaceIssue = {
  readonly kind: "read" | "sync" | "receipt";
  readonly label: string;
  readonly confirmedAt?: number | string | undefined;
  readonly message?: string | undefined;
  readonly retry?: (() => void) | undefined;
};

type RegisterIssue = (id: string, issue: WorkspaceIssue) => () => void;

export const WorkspaceRecoveryContext = createContext<
  { readonly register: RegisterIssue; readonly outage: boolean } | undefined
>(undefined);

export function useWorkspaceRecovery(issue: WorkspaceIssue | undefined) {
  const id = useId();
  const register = useContext(WorkspaceRecoveryContext)?.register;
  const kind = issue?.kind;
  const label = issue?.label;
  const message = issue?.message;
  const confirmedAt = issue?.confirmedAt;
  const retry = issue?.retry;
  const hasRetry = Boolean(retry);
  const latestRetry = useRef(retry);

  useEffect(() => {
    latestRetry.current = retry;
  }, [retry]);
  useEffect(() => {
    if (!register || !kind || !label) return;
    return register(id, {
      kind,
      label,
      message,
      confirmedAt,
      retry: hasRetry ? () => latestRetry.current?.() : undefined,
    });
  }, [id, register, kind, label, message, confirmedAt, hasRetry]);
}
