import type { Launch, LaunchTarget } from "../features/queue/types";

export function SelectionActions({
  targets,
  onLaunch,
  onMonitor,
  onCancel,
}: {
  readonly targets: readonly LaunchTarget[];
  readonly onLaunch: Launch;
  readonly onMonitor: (targets: readonly LaunchTarget[], pause: boolean) => void;
  readonly onCancel: () => void;
}) {
  return (
    <>
      <span className="selection-count" role="status">
        {targets.length} selected
      </span>
      <button
        type="button"
        className="primary"
        disabled={!targets.length}
        onClick={() => onLaunch("prepare", targets)}
      >
        Prepare {targets.length === 1 ? "review" : "reviews"}
      </button>
      <button type="button" disabled={!targets.length} onClick={() => onMonitor(targets, false)}>
        Auto-review updates
      </button>
      <button
        type="button"
        className="quiet-button"
        disabled={!targets.length}
        onClick={() => onMonitor(targets, true)}
      >
        Pause auto-review
      </button>
      <button type="button" className="quiet-button" onClick={onCancel}>
        Cancel
      </button>
    </>
  );
}
