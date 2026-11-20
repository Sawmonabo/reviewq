import type { PlanTask } from "../../api/generated/contracts/history";
import { readPlan } from "../../api/review";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import { Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import "./reported-plan.css";

type Props = {
  readonly runId: string;
  readonly sessionKey: string;
};

export function ReportedPlan({ runId, sessionKey }: Props) {
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
  if (!plan.error && !prose && !tasks?.length) return null;
  return (
    <div className="stack">
      {prose || tasks?.length ? <h3>Agent plan</h3> : null}
      {plan.error && <Notice retry={plan.retry}>{plan.error}</Notice>}
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
      ) : null}
    </div>
  );
}

function readNativePlan(source: string): { tasks: PlanTask[]; explanation?: string } | undefined {
  if (source.length > 64_000) return;
  const first = source.trimStart()[0];
  if (first !== "[" && first !== "{") return;
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
