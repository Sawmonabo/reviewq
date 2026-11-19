import { useState } from "react";
import type { PlanTask } from "../../api/generated/contracts/history";
import { readPlan } from "../../api/review";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import "./reported-plan.css";

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
  const text = plan.data?.plan?.part.text;
  const normalized = plan.data?.tasks?.length ? plan.data.tasks : undefined;
  const native = text && !normalized ? readNativePlan(text) : undefined;
  const tasks = normalized ?? native?.tasks;
  const prose = normalized ? undefined : native ? native.explanation : text;
  return (
    <div className="stack">
      {plan.error && <Notice retry={plan.retry}>{plan.error}</Notice>}
      {plan.pending && <Loading label="Loading reported plan…" />}
      {prose && <MarkdownContent source={prose} />}
      {tasks?.length ? (
        <ol className="reported-plan" role="list">
          {tasks.map((task, index) => (
            <li
              key={task.id}
              data-plan-status={task.status}
              aria-current={task.status === "in_progress" ? "step" : undefined}
            >
              {task.status === "completed" ? (
                <span className="reported-plan-step" role="img" aria-label="Completed">
                  ✓
                </span>
              ) : (
                <span className="reported-plan-step" aria-hidden="true">
                  {index + 1}
                </span>
              )}
              <span>{task.title}</span>
              {task.status === "in_progress" && <small>Current</small>}
            </li>
          ))}
        </ol>
      ) : (
        plan.data && !prose && <p className="meta">No plan was reported by this session.</p>
      )}
    </div>
  );
}

function readNativePlan(source: string): { tasks: PlanTask[]; explanation?: string } | undefined {
  if (!/^\s*[\[{]/.test(source) || source.length > 64_000) return;
  let value: unknown;
  try {
    value = JSON.parse(source);
  } catch {
    return;
  }
  const plan = object(value);
  const rows: unknown = plan?.plan ?? value;
  if (!Array.isArray(rows) || !rows.length || rows.length > 100) return;
  const tasks: PlanTask[] = [];
  for (const [index, row] of rows.entries()) {
    const task = object(row);
    const status = task?.status === "inProgress" ? "in_progress" : task?.status;
    if (
      typeof task?.step !== "string" ||
      !task.step.trim() ||
      (status !== "pending" && status !== "in_progress" && status !== "completed")
    )
      return;
    tasks.push({ id: String(index), title: task.step, status });
  }
  return {
    tasks,
    ...(typeof plan?.explanation === "string" ? { explanation: plan.explanation } : {}),
  };
}

function object(value: unknown): Record<string, unknown> | undefined {
  if (value && typeof value === "object" && !Array.isArray(value))
    return value as Record<string, unknown>;
}
