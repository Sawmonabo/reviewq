import { useContext } from "react";
import { Notice } from "../../../components/PanelState";
import { WorkspaceRecoveryContext } from "../../../hooks/useWorkspaceRecovery";
import { formatTime, readable } from "../../../lib/presentation";

type Status = {
  readonly error: string | undefined;
  readonly connectionError: boolean;
  readonly updated: number | undefined;
  readonly online: boolean;
  readonly availability: string | undefined;
};

export function HistoryStatus({
  history,
  onRetry,
  onReconnect,
}: {
  readonly history: Status;
  readonly onRetry: () => void;
  readonly onReconnect: () => void;
}) {
  const outage = useContext(WorkspaceRecoveryContext)?.outage;
  const showError = history.error && (!outage || !history.connectionError);
  return (
    <>
      {showError && (
        <Notice retry={onRetry}>
          {history.error}
          {history.updated && <> · Last confirmed {formatTime(history.updated)}</>}
        </Notice>
      )}
      {!history.online && !outage && !showError && (
        <Notice retry={onReconnect}>
          Conversation updates paused
          {history.updated ? " · Last confirmed " + formatTime(history.updated) : ""}.
        </Notice>
      )}
      {history.availability && history.availability !== "available" && (
        <Notice>
          {history.availability === "partial"
            ? "Only part of this conversation is available."
            : "Native history · " + readable(history.availability)}
        </Notice>
      )}
    </>
  );
}
