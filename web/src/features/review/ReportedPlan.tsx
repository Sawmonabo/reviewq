import { useState } from "react";
import { readPlan } from "../../api/review";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { readable } from "../../lib/presentation";

export function ReportedPlan({
  runId,
  sessionKey,
}: {
  readonly runId: string;
  readonly sessionKey: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <details onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>Reported plan</summary>
      {open && <PlanDetails runId={runId} sessionKey={sessionKey} />}
    </details>
  );
}

function PlanDetails({
  runId,
  sessionKey,
}: {
  readonly runId: string;
  readonly sessionKey: string;
}) {
  const plan = useResource(
    `plan:${runId}:${sessionKey}`,
    (signal) => readPlan(runId, sessionKey, signal),
    [{ kind: "history", id: sessionKey }],
  );
  return (
    <div className="stack">
      {plan.error && <Notice retry={plan.retry}>{plan.error}</Notice>}
      {plan.pending && <Loading label="Loading reported plan…" />}
      {plan.data?.tasks?.length ? (
        <ol>
          {plan.data.tasks.map((task) => (
            <li key={task.id}>
              {task.title} · {readable(task.status)}
            </li>
          ))}
        </ol>
      ) : plan.data?.plan?.part.text ? (
        <pre className="review-request">{plan.data.plan.part.text}</pre>
      ) : (
        plan.data && <p className="meta">No plan was reported by this session.</p>
      )}
    </div>
  );
}
