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
    ? "off · You approved this MR"
    : monitor.state === "enabled"
      ? "on"
      : readable(monitor.state).toLowerCase();
  return (
    <section className="changes-monitor" aria-label="Automated review">
      <span>
        Auto-review {state} · {count} {count === 1 ? "thread" : "threads"} ·{" "}
        {lead ?? monitor.runId ?? "Assigned lead unavailable"}
      </span>
      {canChange && (
        <>
          <button type="button" className="quiet-button" onClick={onEdit}>
            Edit selected threads
          </button>
          {monitor.state === "enabled" && !approved && (
            <button type="button" className="quiet-button" onClick={onCheck}>
              Review selected updates now
            </button>
          )}
        </>
      )}
      {queued && (
        <small>Update queued{queued.holdReason && ` · ${readable(queued.holdReason)}`}</small>
      )}
    </section>
  );
}
