import { ApiResponseError } from "../api/client";
import type { ResourceKeyDto } from "../api/generated/contracts";
import { ApiTransportError } from "../api/transport-error";
import { useResource } from "./useResource";
import { useWorkspaceRecovery } from "./useWorkspaceRecovery";

export function useWorkspaceResource<T>(
  label: string,
  key: string,
  read: (signal: AbortSignal) => Promise<{ readonly data: T; readonly cursor: string }>,
  resources: readonly ResourceKeyDto[],
  discardOnError?: (error: unknown) => boolean,
) {
  const result = useResource(key, read, resources, discardOnError);
  const connectionFailure = isWorkspaceConnectionFailure(result.errorCause);
  useWorkspaceRecovery(
    connectionFailure
      ? { kind: "read", label, confirmedAt: result.confirmedAt, retry: result.retryIfIdle }
      : undefined,
  );
  return {
    ...result,
    error: connectionFailure ? undefined : result.error,
    unavailable: connectionFailure && result.data === undefined,
  };
}

export function isWorkspaceConnectionFailure(error: unknown) {
  return (
    error instanceof ApiTransportError ||
    (error instanceof ApiResponseError &&
      (error.retryable || error.code === "internal" || error.code === "unauthorized"))
  );
}
