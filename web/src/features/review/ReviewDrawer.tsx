import { lazy, Suspense, useRef, useState } from "react";
import type { ReactNode } from "react";
import { readOverview } from "../../api/client";
import type {
  Disposition,
  MergeRequestRow,
  ReviewOverview,
  RunDetail,
  SessionSummary,
} from "../../api/generated/contracts";
import type { Tab } from "../../api/generated/contracts/notifications";
import {
  cancelRecovery,
  followupEnrollment,
  readRecovery,
  saveDecision,
  terminal,
} from "../../api/review";
import { ActionMenu } from "../../components/ActionMenu";
import { Icon } from "../../components/Icon";
import { PanelBoundary } from "../../components/PanelBoundary";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import type { DraftStore } from "../../lib/drafts";
import { describeError, formatTime, readable, safeUrl } from "../../lib/presentation";
import { ReportedPlan } from "./ReportedPlan";
import { ReviewAttention } from "./ReviewAttention";
import "./review.css";

const AgentPanel = lazy(() => import("./AgentPanel"));
const Changes = lazy(() => import("./changes/Changes"));
const Findings = lazy(() => import("./report/Findings"));

const tabs = [
  ["overview", "Overview"],
  ["agent", "Agent"],
  ["findings", "Findings"],
  ["changes", "Changes"],
] as const;

type Props = {
  readonly overview: ReviewOverview;
  readonly run: RunDetail | undefined;
  readonly session: SessionSummary | undefined;
  readonly requestedSession: string | undefined;
  readonly onDefaultRun: () => void;
  readonly selectionError: string | undefined;
  readonly selectionPending: boolean;
  readonly tab: Tab;
  readonly drafts: DraftStore;
  readonly ticket: ReactNode;
  readonly onTab: (tab: Tab) => void;
  readonly onSession: (session: string | undefined) => void;
  readonly onRun: (run: string) => void;
  readonly onClose: () => void;
  readonly onReload: () => void;
  readonly onLaunch: (mode: "prepare" | "register", targets: readonly MergeRequestRow[]) => void;
  readonly onMonitor: (targets: readonly MergeRequestRow[], pause: boolean) => void;
  readonly onSettings: () => void;
};

export default function ReviewDrawer({
  overview,
  run,
  tab,
  session,
  requestedSession,
  onDefaultRun,
  selectionError,
  selectionPending,
  drafts,
  ticket,
  onTab,
  onSession,
  onRun,
  onClose,
  onReload,
  onLaunch,
  onMonitor,
  onSettings,
}: Props) {
  const { review } = overview;
  const [runTrail, setRunTrail] = useState<readonly string[]>([]);
  const [busy, setBusy] = useState(false);
  const mutation = useRef(false);
  const [outcome, setOutcome] = useState<{ key: string; message: string; terminal?: string }>();
  const buttonRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [acknowledgement, setAcknowledgement] = useState<AttentionSnapshot>();
  const child = session && session.sessionKey !== run?.leadSessionKey ? session : undefined;
  const unavailableSession = Boolean(run && !session && !selectionPending && !selectionError);
  const selected = !selectionPending && !selectionError && !unavailableSession;
  const canMutate = selected && review.mrState === "open";
  const snapshot = captureAttention(overview);
  const changedSinceCapture =
    !canMutate ||
    !snapshot ||
    !acknowledgement ||
    snapshot.checkpointId !== acknowledgement.checkpointId ||
    snapshot.expectedRevision !== acknowledgement.expectedRevision ||
    snapshot.expectedSettingsRevision !== acknowledgement.expectedSettingsRevision ||
    snapshot.sourceSha !== acknowledgement.sourceSha;
  const source = safeUrl(overview.webUrl);
  const scope = [review.reviewId, run?.runId, session?.sessionKey].join("/");
  const outcomeMessage = outcome?.key === scope ? outcome.message : undefined;
  const command = outcome?.key === scope ? outcome.terminal : undefined;
  const recovery = useResource(
    `recovery:${scope}`,
    (signal) =>
      run && session && session.provider === "codex"
        ? readRecovery(run.runId, session.sessionKey, signal)
        : Promise.resolve({ data: null, cursor: "" }),
    run
      ? [
          { kind: "session", id: session?.sessionKey ?? run.leadSessionKey },
          { kind: "run", id: run.runId },
        ]
      : [],
  );
  const follows = useResource(
    `monitor:${review.reviewId}`,
    (signal) => followupEnrollment(review.reviewId, signal),
    [{ kind: "followup", id: review.reviewId }],
  );
  const otherRuns = useResource(
    `review-runs:${review.reviewId}:${runTrail.at(-1) ?? ""}`,
    async (signal) => {
      const cursor = runTrail.at(-1);
      if (!cursor) return { data: null, cursor: "" };
      const result = await readOverview(review.reviewId, signal, cursor);
      return { data: result.data.runs, cursor: result.cursor };
    },
  );
  const runs = runTrail.length ? otherRuns.data : overview.runs;
  const visibleTabs = child
    ? ([
        ["agent", "Conversation"],
        ["findings", "Result"],
      ] as const)
    : tabs;
  const activeTab = child && tab !== "findings" ? "agent" : tab;

  async function commandFor(action: "open" | "copy") {
    if (!selected || !run || !session || mutation.current) return;
    mutation.current = true;
    setBusy(true);
    try {
      const result = await terminal(run.runId, session.sessionKey, action === "open");
      if (result.state === "command") {
        if (action === "copy") await navigator.clipboard.writeText(result.command);
        setOutcome({
          key: scope,
          message:
            action === "copy"
              ? "Terminal command copied."
              : result.opened
                ? "Opened native terminal."
                : "Run this command in the terminal.",
          terminal: result.command,
        });
      } else {
        setOutcome({
          key: scope,
          message: {
            restore_checkout: "Restore the saved working directory to open this session.",
            configure_provider: "Set up the native agent to open this session.",
            foreground_claude: "Bring the existing Claude session to the foreground.",
            inspect_parent: "Open the parent session to inspect this child.",
          }[result.state],
        });
      }
    } catch (error) {
      setOutcome({ key: scope, message: describeError(error) });
    } finally {
      mutation.current = false;
      setBusy(false);
    }
  }

  async function decide(disposition: Disposition) {
    if (mutation.current || !canMutate) return;
    mutation.current = true;
    setBusy(true);
    try {
      const result = await saveDecision(review.reviewId, {
        submissionId: crypto.randomUUID(),
        expectedRevision: overview.decisionRevision,
        expectedSettingsRevision: overview.settingsRevision,
        sourceSha: review.sourceSha,
        disposition,
      });
      setOutcome({
        key: scope,
        message:
          result.disposition === "reviewed"
            ? "Review marked complete."
            : result.disposition === "waiting_author"
              ? "Waiting on author."
              : "Local review record cleared.",
      });
      onReload();
    } catch (error) {
      setOutcome({ key: scope, message: describeError(error) });
    } finally {
      mutation.current = false;
      setBusy(false);
    }
  }

  async function confirmAttention() {
    if (!acknowledgement || changedSinceCapture || mutation.current) return;
    const { checkpointId, expectedRevision, expectedSettingsRevision, sourceSha } = acknowledgement;
    mutation.current = true;
    setBusy(true);
    try {
      await saveDecision(review.reviewId, {
        submissionId: crypto.randomUUID(),
        checkpointId,
        expectedRevision,
        expectedSettingsRevision,
        sourceSha,
      });
      setAcknowledgement(undefined);
      setOutcome({ key: scope, message: "Follow-up checkpoint saved." });
      onReload();
    } catch (error) {
      setAcknowledgement(undefined);
      setOutcome({ key: scope, message: describeError(error) });
      onReload();
    } finally {
      mutation.current = false;
      setBusy(false);
    }
  }

  async function stopRecovery() {
    const episode = recovery.data;
    if (!selected || !run || !session || !episode || mutation.current) return;
    mutation.current = true;
    setBusy(true);
    try {
      const result = await cancelRecovery(run.runId, session.sessionKey, {
        episodeId: episode.episodeId,
        expectedRevision: episode.revision,
      });
      setOutcome({ key: scope, message: `Recovery · ${readable(result.state)}` });
      recovery.retry();
    } catch (error) {
      setOutcome({ key: scope, message: describeError(error) });
    } finally {
      mutation.current = false;
      setBusy(false);
    }
  }

  function activate(next: Tab) {
    onTab(next);
  }

  return (
    <aside className="drawer-surface" aria-labelledby="review-title">
      <header className="drawer-header">
        <div className="drawer-header-row">
          <div className="drawer-breadcrumb">
            <button
              type="button"
              className="quiet-button ticket-key"
              onClick={() => {
                if (child) onSession(undefined);
                onTab("overview");
              }}
            >
              {review.tickets.map((item) => item.key).join(" · ") || `!${review.iid}`}
            </button>
            {review.tickets.length > 0 && (
              <>
                <span aria-hidden="true">/</span>
                {source ? (
                  <a href={source} target="_blank" rel="noreferrer">
                    !{review.iid}
                  </a>
                ) : (
                  <span>!{review.iid}</span>
                )}
              </>
            )}
            {child && (
              <>
                <span aria-hidden="true">›</span>
                <button type="button" className="quiet-button" onClick={() => onSession(undefined)}>
                  Lead review
                </button>
                <span aria-hidden="true">›</span>
                <span>Subagent</span>
              </>
            )}
          </div>
          <div className="actions drawer-actions">
            <ActionMenu
              label="Review actions"
              trigger={<Icon name="more" />}
              items={[
                {
                  label: "Open in terminal",
                  run: () => void commandFor("open"),
                  disabled: !selected || !session || busy,
                },
                {
                  label: "Copy terminal command",
                  run: () => void commandFor("copy"),
                  disabled: !selected || !session || busy,
                },
                {
                  label: "Open MR in GitLab",
                  run: () => {
                    if (source) window.open(source, "_blank", "noopener,noreferrer");
                  },
                  disabled: !source,
                },
                {
                  label: "Cancel auto-resume",
                  run: () => void stopRecovery(),
                  disabled:
                    !selected ||
                    !recovery.data ||
                    !["waiting", "queued"].includes(recovery.data.state) ||
                    busy,
                },
                {
                  label:
                    follows.data?.monitor?.state === "enabled"
                      ? "Edit monitored discussions"
                      : "Configure auto-review",
                  run: () => onMonitor([review], false),
                  disabled: !canMutate || busy,
                },
                {
                  label: "Pause auto-review",
                  run: () => onMonitor([review], true),
                  disabled: !canMutate || follows.data?.monitor?.state !== "enabled" || busy,
                },
                {
                  label: "Review follow-up evidence",
                  run: () => {
                    if (snapshot) setAcknowledgement(snapshot);
                  },
                  disabled: !canMutate || !snapshot || busy,
                },
                {
                  label: "Review complete",
                  run: () => void decide("reviewed"),
                  disabled: !canMutate || busy,
                },
                {
                  label: "Waiting on author",
                  run: () => void decide("waiting_author"),
                  disabled: !canMutate || busy,
                },
                {
                  label: "Clear local review record",
                  run: () => void decide("needs_review"),
                  disabled: !canMutate || busy,
                },
              ]}
            />
            <button
              type="button"
              className="icon-button"
              aria-label="Close review"
              onClick={onClose}
            >
              <Icon name="close" />
            </button>
          </div>
        </div>
        <div className="drawer-heading">
          <h2
            id="review-title"
            title={child ? "Subagent" : (review.tickets[0]?.title ?? review.title)}
          >
            {child ? "Subagent" : (review.tickets[0]?.title ?? review.title)}
          </h2>
          <DrawerSummary review={review} run={run} child={Boolean(child)} />
        </div>
        {(Boolean(runs?.nextCursor) || runTrail.length > 0) && (
          <div className="actions run-pages">
            {runTrail.length > 0 && (
              <button type="button" onClick={() => setRunTrail(runTrail.slice(0, -1))}>
                Previous runs
              </button>
            )}
            {runs?.nextCursor && (
              <button
                type="button"
                onClick={() => {
                  if (runs.nextCursor) setRunTrail([...runTrail, runs.nextCursor]);
                }}
              >
                More runs
              </button>
            )}
          </div>
        )}
        {otherRuns.error && <Notice retry={otherRuns.retry}>{otherRuns.error}</Notice>}
        <div className="drawer-tab-bar">
          <div
            role="tablist"
            aria-label={child ? "Child session" : "Review details"}
            className="drawer-tabs"
            onKeyDown={(event) => {
              const current = visibleTabs.findIndex(([id]) => id === activeTab);
              const last = visibleTabs.length - 1;
              const next =
                event.key === "Home"
                  ? 0
                  : event.key === "End"
                    ? last
                    : event.key === "ArrowLeft"
                      ? (current + last) % visibleTabs.length
                      : event.key === "ArrowRight"
                        ? (current + 1) % visibleTabs.length
                        : -1;
              const item = visibleTabs[next];
              if (!item) return;
              event.preventDefault();
              activate(item[0]);
              buttonRefs.current[next]?.focus();
            }}
          >
            {visibleTabs.map(([id, label], index) => (
              <button
                type="button"
                role="tab"
                id={`review-tab-${id}`}
                key={id}
                ref={(element) => {
                  buttonRefs.current[index] = element;
                }}
                aria-controls="review-tabpanel"
                aria-selected={id === activeTab}
                tabIndex={id === activeTab ? 0 : -1}
                onClick={() => activate(id)}
              >
                {label}
              </button>
            ))}
          </div>{" "}
          {!child &&
            ((runs?.items.length ?? 0) > 1 ||
              (!selected && Boolean(runs?.items.length)) ||
              runTrail.length > 0 ||
              Boolean(runs?.nextCursor)) && (
              <select
                className="run-picker"
                aria-label="Runs"
                value={run?.runId ?? ""}
                onChange={(event) => {
                  if (event.currentTarget.value) onRun(event.currentTarget.value);
                }}
              >
                <option value="" disabled>
                  Choose run
                </option>
                {run && !runs?.items.some((item) => item.runId === run.runId) && (
                  <option value={run.runId}>Selected run</option>
                )}
                {runs?.items.map((item) => (
                  <option value={item.runId} key={item.runId}>
                    {item.provider === "claude" ? "Claude Code" : "Codex"} ·{" "}
                    {item.startedAt ? formatTime(item.startedAt) : item.runId}
                  </option>
                ))}
              </select>
            )}
        </div>
      </header>
      <div
        id="review-tabpanel"
        role="tabpanel"
        aria-labelledby={`review-tab-${activeTab}`}
        className={activeTab === "agent" ? "drawer-body agent-body" : "drawer-body"}
      >
        {outcomeMessage && (
          <Notice>
            {outcomeMessage} {command && <code>{command}</code>}
          </Notice>
        )}
        {selectionError && (
          <Notice retry={onReload}>
            The requested selection is unavailable. {selectionError}{" "}
            <button type="button" onClick={onDefaultRun}>
              Open default run
            </button>
          </Notice>
        )}
        {unavailableSession && (
          <Notice>
            {requestedSession ? "Child session unavailable." : "Lead session unavailable."}{" "}
            {requestedSession && (
              <button type="button" onClick={() => onSession(undefined)}>
                Return to lead
              </button>
            )}
          </Notice>
        )}
        <PanelBoundary key={activeTab}>
          <Suspense fallback={<Loading label={`Opening ${activeTab}…`} />}>
            {activeTab === "overview" && (
              <Overview
                overview={overview}
                run={run}
                ticket={ticket}
                monitor={follows.data?.monitor?.state}
                onLaunch={onLaunch}
                onSettings={onSettings}
                pending={selectionPending}
                selected={selected}
              />
            )}
            {activeTab === "changes" && (
              <Changes key={review.reviewId} reviewId={review.reviewId} webUrl={overview.webUrl} />
            )}
            {activeTab === "agent" &&
              (selected && run && session ? (
                <AgentPanel
                  key={scope}
                  run={run}
                  session={session}
                  reviewTitle={review.title}
                  state={review.focusedRunId === run.runId ? review.runStage : null}
                  drafts={drafts}
                  onSession={onSession}
                  onTerminal={() => void commandFor("open")}
                />
              ) : selectionPending ? (
                <Loading label="Resolving selected run…" />
              ) : (
                <Empty title="No confirmed conversation" />
              ))}
            {activeTab === "findings" &&
              (selected && child && run ? (
                <AgentPanel
                  key={`${scope}:result`}
                  run={run}
                  session={child}
                  reviewTitle={review.title}
                  state={review.focusedRunId === run.runId ? review.runStage : null}
                  drafts={drafts}
                  onSession={onSession}
                  onTerminal={() => void commandFor("open")}
                  resultOnly
                />
              ) : selected && run && session ? (
                <Findings
                  key={run.runId}
                  run={run}
                  drafts={drafts}
                  reviewTitle={review.title}
                  webUrl={overview.webUrl}
                  followup={follows.data?.latestCompletedResult}
                  onRun={onRun}
                />
              ) : selectionPending ? (
                <Loading label="Resolving selected run…" />
              ) : (
                <Empty
                  title={
                    selected ? "No registered review report" : "Review run selection unavailable"
                  }
                />
              ))}
          </Suspense>
        </PanelBoundary>
      </div>
      {acknowledgement && (
        <ReviewAttention
          attention={acknowledgement.attention}
          stale={changedSinceCapture}
          busy={busy}
          source={source}
          onClose={() => setAcknowledgement(undefined)}
          onConfirm={() => void confirmAttention()}
          onRefresh={() => setAcknowledgement(snapshot)}
        />
      )}
    </aside>
  );
}

function DrawerSummary({
  review,
  run,
  child,
}: {
  readonly review: MergeRequestRow;
  readonly run: RunDetail | undefined;
  readonly child: boolean;
}) {
  if (child) return <p className="drawer-meta">Native subagent conversation and result</p>;
  const reason = run && review.focusedRunId === run.runId ? review.reasonKind : null;
  const known: Readonly<Record<string, readonly [string, string]>> = {
    agent_input: ["Agent needs input", "rose"],
    permission_required: ["Agent needs input", "rose"],
    report_ready: ["Response ready", "blue"],
    report_failed: ["Findings could not be saved", "red"],
    lead_working: ["Working", "green"],
    session_working: ["Working", "green"],
    waiting_author: ["Waiting on author", "amber"],
    discussion_update: ["Follow-up ready", "blue"],
    decision_required: ["Review ready", "blue"],
  };
  const state = reason ? (known[reason] ?? [readable(reason), ""]) : undefined;
  return (
    <div className="drawer-meta">
      {state && <span className={`drawer-state badge ${state[1]}`}>{state[0]}</span>}
      <span>
        {[review.author?.label, review.projectPath.split("/").at(-1)].filter(Boolean).join(" · ")}
      </span>
      {review.personalApproval && <span className="drawer-fact approved">You approved</span>}
      {review.ourComments > 0 && (
        <span className="drawer-fact comments">Commented · {review.ourComments}</span>
      )}
      {review.unreadReplies > 0 && (
        <span className="drawer-fact replies">
          {review.unreadReplies === 1 ? "New reply" : "New replies"} · {review.unreadReplies}
        </span>
      )}
    </div>
  );
}

function Overview({
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
  readonly onLaunch: Props["onLaunch"];
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
              <code>{review.sourceSha}</code>
            </dd>
          </div>
          <div>
            <dt>Reviewed revision</dt>
            <dd>
              <code>{overview.decision?.reviewedSha ?? review.fullMrReviewedSha ?? "Unknown"}</code>
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
                  <code>{run.launch.sourceSha}</code>
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

type AttentionSnapshot = {
  readonly checkpointId: string;
  readonly expectedRevision: string;
  readonly expectedSettingsRevision: string;
  readonly sourceSha: string;
  readonly attention: NonNullable<ReviewOverview["attention"]>;
};

function captureAttention(overview: ReviewOverview): AttentionSnapshot | undefined {
  const { attention, checkpointId, decisionRevision, settingsRevision, review } = overview;
  if (
    !checkpointId ||
    !attention ||
    (!attention.source.length && !attention.sessions.length && !attention.reports.length)
  )
    return;
  return {
    checkpointId,
    expectedRevision: decisionRevision,
    expectedSettingsRevision: settingsRevision,
    sourceSha: review.sourceSha,
    attention,
  };
}
