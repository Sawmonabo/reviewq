import type { ReactNode } from "react";
import type { MergeRequestRow, ReviewOverview, RunDetail } from "../../api/generated/contracts";
import { Empty, Loading } from "../../components/PanelState";
import { readable, safeUrl } from "../../lib/presentation";
import { ReportedPlan } from "./ReportedPlan";

export function ReviewOverviewPane({
  overview,
  run,
  ticket,
  monitor,
  onLaunch,
  onSettings,
  pending,
  selected,
}: {
  readonly overview: ReviewOverview;
  readonly run: RunDetail | undefined;
  readonly ticket: ReactNode;
  readonly monitor: string | undefined;
  readonly onLaunch: (mode: "prepare" | "register", targets: readonly MergeRequestRow[]) => void;
  readonly onSettings: () => void;
  readonly pending: boolean;
  readonly selected: boolean;
}) {
  const { review } = overview;
  const source = safeUrl(overview.webUrl);
  return (
    <div className="overview stack">
      {ticket}
      <section className="overview-review-details stack" aria-label="Review details">
        <div className="spread">
          <h3>Review details</h3>
          <span className="badge">{readable(review.mrState)}</span>
        </div>
        <p>
          <strong>{review.title}</strong>{" "}
          {source && (
            <a href={source} target="_blank" rel="noreferrer">
              GitLab ↗
            </a>
          )}
        </p>
        <dl className="overview-facts">
          <div>
            <dt>Project</dt>
            <dd>
              {review.projectPath}!{review.iid}
            </dd>
          </div>
          <div>
            <dt>Author</dt>
            <dd>{review.author?.label ?? "Unknown"}</dd>
          </div>
          <div>
            <dt>Source revision</dt>
            <dd>
              <code title={review.sourceSha}>{review.sourceSha.slice(0, 12)}</code>
            </dd>
          </div>
          <div>
            <dt>Reviewed revision</dt>
            <dd>
              <code title={overview.decision?.reviewedSha ?? review.fullMrReviewedSha ?? undefined}>
                {(overview.decision?.reviewedSha ?? review.fullMrReviewedSha)?.slice(0, 12) ??
                  "Not recorded"}
              </code>
            </dd>
          </div>
          <div>
            <dt>Review state</dt>
            <dd>{readable(overview.decision?.disposition ?? review.queueState)}</dd>
          </div>
          <div>
            <dt>Automated follow-up</dt>
            <dd>{monitor ? readable(monitor) : "No enrollment"}</dd>
          </div>
        </dl>
      </section>
      {pending && <Loading label="Resolving selected run…" />}
      {run ? (
        <section className="overview-review-details stack" aria-label="Selected run">
          <h3>Selected run</h3>
          <dl className="overview-facts">
            <div>
              <dt>Run</dt>
              <dd>
                <code>{run.runId}</code>
              </dd>
            </div>
            <div>
              <dt>Lead native session</dt>
              <dd>
                <code>{run.leadNativeId}</code>
              </dd>
            </div>
            <div>
              <dt>Working directory</dt>
              <dd>
                <code>{run.leadCwd}</code>
              </dd>
            </div>
            <div>
              <dt>Model</dt>
              <dd>{run.launch?.observedModel ?? run.launch?.requestedModel ?? "Unavailable"}</dd>
            </div>
            <div>
              <dt>Effort</dt>
              <dd>{run.launch?.observedEffort ?? run.launch?.requestedEffort ?? "Unavailable"}</dd>
            </div>
            {run.launch && (
              <div>
                <dt>Starting revision</dt>
                <dd>
                  <code title={run.launch.sourceSha}>{run.launch.sourceSha.slice(0, 12)}</code>
                </dd>
              </div>
            )}
          </dl>
          {run.launch && (
            <details>
              <summary>Original review request</summary>
              <pre className="review-request">
                {run.launch.invocation.kind === "custom"
                  ? run.launch.invocation.prompt
                  : `${run.launch.invocation.name} ${run.launch.invocation.args ?? ""}`}
                {run.launch.context && `\n\n${run.launch.context}`}
              </pre>
            </details>
          )}
          <ReportedPlan runId={run.runId} sessionKey={run.leadSessionKey} />
        </section>
      ) : (
        selected && (
          <Empty title="No registered run">
            <div className="actions">
              {review.mrState === "open" && (
                <button
                  type="button"
                  className="primary"
                  onClick={() => onLaunch("prepare", [review])}
                >
                  Prepare review
                </button>
              )}
              <button type="button" onClick={() => onLaunch("register", [review])}>
                Register run
              </button>
              <button type="button" onClick={onSettings}>
                Settings
              </button>
            </div>
          </Empty>
        )
      )}
    </div>
  );
}
