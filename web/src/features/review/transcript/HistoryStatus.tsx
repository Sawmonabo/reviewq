import { Notice } from "../../../components/PanelState";
import { formatTime, readable } from "../../../lib/presentation";

type Status = {
  readonly error: string | undefined;
  readonly updated: number | undefined;
  readonly online: boolean;
  readonly availability: string | undefined;
};

export function HistoryStatus({
  history,
  onRetry,
}: {
  readonly history: Status;
  readonly onRetry: () => void;
}) {
  return (
    <>
      {history.error && (
        <Notice retry={onRetry}>
          {history.error}
          {history.updated && <> · Last confirmed {formatTime(history.updated)}</>}
        </Notice>
      )}
      {!history.online && (
        <Notice>
          Live updates paused
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
