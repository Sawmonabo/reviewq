import type { ReactNode } from "react";
import type { MergeRequestRow, ReviewOverview, RunDetail } from "../../api/generated/contracts";
import { Empty, Loading } from "../../components/PanelState";
import { readable, safeUrl } from "../../lib/presentation";
import { InterruptSession } from "./InterruptSession";
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
  const discussions = overview.ourDiscussions;
  return (
    <div className="overview stack">
      {ticket}
      <section className="overview-review-details stack" aria-label="Review details">
        <div className="spread">
          <h3>Review details</h3>
          <span className="badge">{readable(review.mrState)}</span>
        </div>
        <dl className="overview-facts">
          <div>
            <dt>MR title</dt>
            <dd>
              {source ? (
                <a href={source} target="_blank" rel="noreferrer">
                  {review.title} ↗
                </a>
              ) : (
                review.title
              )}
            </dd>
          </div>
          <div>
            <dt>Registration key</dt>
            <dd>
              <code>{review.reviewId}</code>
            </dd>
          </div>
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
            <dt>Your last review</dt>
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
            <dt>Our discussions</dt>
            <dd>
              {discussions
                ? `${discussions.total} total · ${discussions.open === null ? "Open count unknown" : `${discussions.open} open`}`
                : "Not observed"}
            </dd>
          </div>
          <div>
            <dt>Our comments</dt>
            <dd>
              {discussions || review.ourComments > 0
                ? `${review.ourComments} posted on GitLab`
                : "Not observed"}
            </dd>
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
              <dt>Requested model</dt>
              <dd>{run.launch ? (run.launch.requestedModel ?? "CLI default") : "Not recorded"}</dd>
            </div>
            <div>
              <dt>Requested effort</dt>
              <dd>{run.launch ? (run.launch.requestedEffort ?? "CLI default") : "Not recorded"}</dd>
            </div>
            <div>
              <dt>Reported model</dt>
              <dd>{run.leadMetadata.model ?? run.launch?.observedModel ?? "Not reported yet"}</dd>
            </div>
            <div>
              <dt>Reported effort</dt>
              <dd>{run.leadMetadata.effort ?? run.launch?.observedEffort ?? "Not reported yet"}</dd>
            </div>
            <div>
              <dt>Skill requested</dt>
              <dd>
                {run.launch?.invocation.kind === "skill"
                  ? run.launch.invocation.name
                  : run.launch
                    ? "Custom prompt"
                    : "Not recorded"}
              </dd>
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
          {selected && run.launch && run.launch.state !== "rejected" && (
            <InterruptSession
              key={run.leadSessionKey}
              runId={run.runId}
              nativeId={run.leadNativeId}
              sessionKey={run.leadSessionKey}
            />
          )}
        </section>
      ) : (
        selected && (
          <Empty title="No review run registered">
            <p>Start a review or connect the conversation already working on this MR.</p>
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
                Register existing session
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
