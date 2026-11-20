import type { ReactNode } from "react";
import type { MergeRequestRow, ReviewOverview, RunDetail } from "../../api/generated/contracts";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import { Empty, Loading } from "../../components/PanelState";
import { readable } from "../../lib/presentation";
import { InterruptSession } from "./InterruptSession";
import { NativeReviewInput } from "./NativeReviewInput";
import { ReportedPlan } from "./ReportedPlan";

const queueLabels: Readonly<Record<string, string>> = {
  needs_review: "Needs review",
  followup: "Follow-up needed",
  waiting_author: "Waiting on author",
  reviewed: "Reviewed · awaiting merge",
  archive: "Archive",
};

export function ReviewOverviewPane({
  overview,
  run,
  ticket,
  onLaunch,
  pending,
  selected,
}: {
  readonly overview: ReviewOverview;
  readonly run: RunDetail | undefined;
  readonly ticket: ReactNode;
  readonly onLaunch: (mode: "prepare" | "register", targets: readonly MergeRequestRow[]) => void;
  readonly pending: boolean;
  readonly selected: boolean;
}) {
  const { review, ourDiscussions: discussions } = overview;
  const reviewed = overview.decision?.reviewedSha ?? review.fullMrReviewedSha;
  const launch = run?.launch;
  const evidence = run?.requestEvidence;
  const requestSettings = launch ?? evidence;
  return (
    <div className="overview stack">
      {ticket}
      <section className="overview-review-details stack" aria-label="Review details">
        <h3>Review details</h3>
        <dl className="overview-facts">
          <div>
            <dt>MR title</dt>
            <dd>{review.title}</dd>
          </div>
          <div>
            <dt>Registration key</dt>
            <dd>
              <code>{review.reviewId}</code>
            </dd>
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
              {reviewed ? <code title={reviewed}>{reviewed.slice(0, 12)}</code> : "Not recorded"}
            </dd>
          </div>
          <div>
            <dt>Review state</dt>
            <dd>{queueLabels[review.queueState] ?? readable(review.queueState)}</dd>
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
        </dl>
        {pending && <Loading label="Loading review details…" />}
        {run ? (
          <>
            <dl className="overview-facts">
              <div>
                <dt>Run</dt>
                <dd>
                  <code>{run.runId}</code>
                </dd>
              </div>
              <div>
                <dt>Session</dt>
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
                <dd>
                  {requestSettings
                    ? (requestSettings.requestedModel ?? "CLI default")
                    : "Not recorded"}
                </dd>
              </div>
              <div>
                <dt>Requested effort</dt>
                <dd>
                  {requestSettings
                    ? (requestSettings.requestedEffort ?? "CLI default")
                    : "Not recorded"}
                </dd>
              </div>
              <div>
                <dt>Reported model</dt>
                <dd>{run.leadMetadata.model ?? launch?.observedModel ?? "Not reported yet"}</dd>
              </div>
              <div>
                <dt>Reported effort</dt>
                <dd>{run.leadMetadata.effort ?? launch?.observedEffort ?? "Not reported yet"}</dd>
              </div>
              <div>
                <dt>Skill requested</dt>
                <dd>
                  {launch?.invocation.kind === "skill"
                    ? launch.invocation.name
                    : launch
                      ? "Custom prompt"
                      : (evidence?.skillName ?? "Not recorded")}
                </dd>
              </div>
              {launch && launch.sourceSha !== review.sourceSha && (
                <div>
                  <dt>Starting revision</dt>
                  <dd>
                    <code title={launch.sourceSha}>{launch.sourceSha.slice(0, 12)}</code>
                  </dd>
                </div>
              )}
            </dl>
            {launch ? (
              <ReviewRequest launch={launch} />
            ) : (
              evidence?.input && (
                <NativeReviewInput
                  target={{ runId: run.runId, sessionKey: run.leadSessionKey }}
                  input={evidence.input}
                />
              )
            )}
            <ReportedPlan runId={run.runId} sessionKey={run.leadSessionKey} />
            {selected && launch && launch.state !== "rejected" && (
              <InterruptSession
                key={run.leadSessionKey}
                runId={run.runId}
                nativeId={run.leadNativeId}
                sessionKey={run.leadSessionKey}
              />
            )}
          </>
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
              </div>
            </Empty>
          )
        )}
      </section>
    </div>
  );
}

function ReviewRequest({ launch }: { readonly launch: NonNullable<RunDetail["launch"]> }) {
  const args = launch.invocation.kind === "skill";
  const request =
    launch.invocation.kind === "skill" ? launch.invocation.args : launch.invocation.prompt;
  const sharesRequest = Boolean(
    launch.context?.trim() && launch.context.trim() === request?.trim(),
  );
  const context = sharesRequest ? undefined : launch.context;
  return (
    <>
      {request?.trim() && (
        <section className="review-request">
          <h3>{args && !sharesRequest ? "Skill arguments" : "Review request"}</h3>
          <MarkdownContent source={request} />
        </section>
      )}
      {context?.trim() && (
        <section className="review-request">
          <h3>Additional context</h3>
          <MarkdownContent source={context} />
        </section>
      )}
    </>
  );
}
