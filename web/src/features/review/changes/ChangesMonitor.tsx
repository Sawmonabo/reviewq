import { useId } from "react";
import type { Enrollment } from "../../../api/generated/contracts/followups";
import { readable } from "../../../lib/presentation";

export function ChangesMonitor({
  enrollment,
  approved,
  lead,
  canChange,
  onEdit,
  onCheck,
}: {
  readonly enrollment: Enrollment | undefined;
  readonly approved: boolean | null;
  readonly lead: string | undefined;
  readonly canChange: boolean;
  readonly onEdit: () => void;
  readonly onCheck: () => void;
}) {
  const id = useId();
  const monitor = enrollment?.monitor;
  if (!monitor) return null;
  const count = new Set(
    monitor.selections.flatMap(({ selected, discussionId }) =>
      selected && discussionId ? [discussionId] : [],
    ),
  ).size;
  const queued = enrollment?.batches.find(
    ({ state }) => state === "pending" || state === "reserved",
  );
  const state = approved
    ? "off"
    : monitor.state === "enabled"
      ? "on"
      : readable(monitor.state).toLowerCase();
  return (
    <div className="changes-monitor">
      <button type="button" popoverTarget={id} aria-haspopup="dialog" className="quiet-button">
        Auto-review {state}
        {queued && " · Update queued"}
      </button>
      <section
        id={id}
        popover="auto"
        role="dialog"
        aria-label="Automated review"
        className="changes-popover stack"
      >
        <h3>Auto-review {state}</h3>
        <p>
          {count} {count === 1 ? "thread" : "threads"} ·{" "}
          {lead ?? monitor.runId ?? "Assigned lead unavailable"}
        </p>
        {approved && <p>You approved this MR.</p>}
        {queued && <p>Update queued{queued.holdReason && ` · ${readable(queued.holdReason)}`}</p>}
        {canChange && (
          <div className="actions">
            <button type="button" onClick={onEdit} popoverTarget={id} popoverTargetAction="hide">
              Edit selected threads
            </button>
            {monitor.state === "enabled" && !approved && (
              <button type="button" onClick={onCheck} popoverTarget={id} popoverTargetAction="hide">
                Review selected updates now
              </button>
            )}
          </div>
        )}
      </section>
    </div>
  );
}
