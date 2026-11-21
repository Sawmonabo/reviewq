import { Suspense, useMemo, useRef, useState } from "react";
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
  changeFollowup,
  followupEnrollment,
  readRecovery,
  saveDecision,
  terminal,
} from "../../api/review";
import { ActionMenu } from "../../components/ActionMenu";
import { FilterSelect } from "../../components/FilterSelect";
import { DiagramCacheContext } from "../../components/markdown/diagram-cache";
import { Icon } from "../../components/Icon";
import { PanelBoundary } from "../../components/PanelBoundary";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import type { DraftStore } from "../../lib/drafts";
import { describeError, formatActivityTime, readable, safeUrl } from "../../lib/presentation";
import AgentPanel from "./AgentPanel";
import { ReviewOverviewPane } from "./ReviewOverviewPane";
import { ReviewAttention } from "./ReviewAttention";
import Changes from "./changes/Changes";
import { ChangesMonitor } from "./changes/ChangesMonitor";
import Findings from "./report/Findings";
import SessionResult, { useSessionResult } from "./transcript/SessionResult";
import { useDrawerScroll } from "./useDrawerScroll";
import "./review.css";

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
  readonly requestedRun: string | undefined;
  readonly requestedSession: string | undefined;
  readonly onDefaultRun: () => void;
  readonly selectionError: string | undefined;
  readonly selectionPending: boolean;
  readonly tab: Tab;
  readonly drafts: DraftStore;
  readonly ticket: ReactNode;
  readonly onTab: (tab: Tab) => void;
  readonly onSession: (session: string | undefined, tab?: Tab) => void;
  readonly onRun: (run: string) => void;
  readonly onClose: () => void;
  readonly onReload: () => void;
  readonly onLaunch: (mode: "prepare" | "register", targets: readonly MergeRequestRow[]) => void;
  readonly onMonitor: (targets: readonly MergeRequestRow[], pause: boolean) => void;
};

export default function ReviewDrawer({
  overview,
  run,
  tab,
  session,
  requestedRun,
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
}: Props) {
  const { review } = overview;
  const [runTrail, setRunTrail] = useState<readonly string[]>([]);
  const [deliveryScope, setDeliveryScope] = useState<string>();
  const [busy, setBusy] = useState(false);
  const mutation = useRef(false);
  const [outcome, setOutcome] = useState<{ key: string; message: string; terminal?: string }>();
  const buttonRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [acknowledgement, setAcknowledgement] = useState<AttentionSnapshot>();
  const child = session && session.sessionKey !== run?.leadSessionKey ? session : undefined;
  const title = child
    ? (child.metadata.name ?? "Subagent")
    : (review.tickets[0]?.title ?? review.title);
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
  const diagrams = useMemo(() => ({ scope }), [scope]);
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
  const monitor = follows.data?.monitor;
  const monitorEnabled = monitor?.state === "enabled";
  const recoveryActive = recovery.data && ["waiting", "queued"].includes(recovery.data.state);
  const disposition =
    overview.decision?.reviewedSha === review.sourceSha ? overview.decision.disposition : null;
  const monitorLead = overview.runs.items.find((item) => item.runId === monitor?.runId);
  const startedAt = overview.runs.items.find((item) => item.runId === run?.runId)?.startedAt;
  const visibleTabs = child
    ? ([
        ["agent", "Conversation"],
        ["findings", "Result"],
      ] as const)
    : tabs;
  const activeTab = child && tab !== "findings" ? "agent" : tab;
  const childTarget = run && child ? { runId: run.runId, sessionKey: child.sessionKey } : undefined;
  const childResult = useSessionResult(
    child &&
      (activeTab === "findings" ||
        child.metadata.status === "failed" ||
        child.metadata.status === "system_error")
      ? childTarget
      : undefined,
  );
  const scrollKey =
    activeTab === "overview" || (activeTab === "findings" && child)
      ? `${scope}/${activeTab}`
      : undefined;
  const { scroller: scrollerRef, content: setReadingContent } = useDrawerScroll(scrollKey);

  async function commandFor(action: "open" | "copy") {
    if (!selected || !run || !session || mutation.current) return;
    mutation.current = true;
    setBusy(true);
    try {
      const result = await terminal(run.runId, session.sessionKey, action === "open");
      if (result.state === "command") {
        let message = result.opened
          ? "Opened native terminal."
          : "Run this command in the terminal.";
        if (action === "copy") {
          try {
            await navigator.clipboard.writeText(result.command);
            message = "Terminal command copied.";
          } catch {
            message = "Copy failed. Copy the exact session command below.";
          }
        }
        setOutcome({
          key: scope,
          message,
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
    if (disposition !== "needs_review" && snapshot) {
      setAcknowledgement({ ...snapshot, disposition });
      return;
    }
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
    const { checkpointId, expectedRevision, expectedSettingsRevision, sourceSha, disposition } =
      acknowledgement;
    mutation.current = true;
    setBusy(true);
    try {
      const basis = {
        submissionId: crypto.randomUUID(),
        checkpointId,
        expectedRevision,
        expectedSettingsRevision,
        sourceSha,
      };
      await saveDecision(review.reviewId, disposition ? { ...basis, disposition } : basis);
      setAcknowledgement(undefined);
      setOutcome({
        key: scope,
        message:
          disposition === "reviewed"
            ? "Review marked complete."
            : disposition === "waiting_author"
              ? "Waiting on author."
              : "Follow-up checkpoint saved.",
      });
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

  async function checkMonitoredChanges() {
    if (
      mutation.current ||
      !canMutate ||
      monitor?.state !== "enabled" ||
      review.personalApproval === true
    )
      return;
    mutation.current = true;
    setBusy(true);
    try {
      const result = await changeFollowup(review.reviewId, "review-now", {
        expectedGeneration: monitor.generation,
        submissionId: crypto.randomUUID(),
      });
      setOutcome({
        key: scope,
        message: result.batchId
          ? "Selected updates queued for the review agent."
          : `Auto-review · ${readable(result.state)}`,
      });
      follows.retry();
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

  const drawer = (
    <aside className="drawer-surface" data-tab={activeTab} aria-labelledby="review-title">
      <header className="drawer-header">
        <div className="drawer-header-row">
          <div className="drawer-breadcrumb">
            <button
              type="button"
              className="quiet-button ticket-key"
              onClick={() => {
                if (child) onSession(undefined, "overview");
                else onTab("overview");
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
          {!child &&
            ((runs?.items.length ?? 0) > 1 ||
              (!selected && Boolean(runs?.items.length)) ||
              runTrail.length > 0 ||
              Boolean(runs?.nextCursor)) && (
              <FilterSelect
                label="Runs"
                value={run?.runId ?? ""}
                options={[
                  ...(!run ? [{ value: "", label: "Choose run", disabled: true }] : []),
                  ...(run && !runs?.items.some((item) => item.runId === run.runId)
                    ? [{ value: run.runId, label: "Selected run" }]
                    : []),
                  ...(runs?.items ?? []).map((item) => ({
                    value: item.runId,
                    label: [
                      item.provider === "claude" ? "Claude Code" : "Codex",
                      item.startedAt ? formatActivityTime(item.startedAt) : item.runId,
                    ]
                      .filter(Boolean)
                      .join(" · "),
                  })),
                ]}
                onChange={onRun}
              />
            )}
          <div className="actions drawer-actions">
            <ActionMenu
              label="Review actions"
              trigger={<Icon name="more" />}
              contentClassName="review-action-menu"
              items={[
                ...(recoveryActive
                  ? [
                      {
                        label: "Cancel auto-resume",
                        run: () => void stopRecovery(),
                        disabled: !selected || busy,
                      },
                    ]
                  : []),
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
                  ...(source ? { href: source, target: "_blank", rel: "noreferrer" } : {}),
                  disabled: !source,
                  trailing: <Icon name="external" size={12} />,
                },
                ...(activeTab === "agent" && selected && run && session && !child
                  ? [
                      {
                        label:
                          deliveryScope === scope ? "Hide delivery history" : "Delivery history",
                        run: () => setDeliveryScope(deliveryScope === scope ? undefined : scope),
                      },
                    ]
                  : []),
                ...(review.mrState === "open"
                  ? [
                      {
                        label: monitorEnabled
                          ? "Disable automatic follow-ups for this MR"
                          : "Enable automatic follow-ups for this MR",
                        run: () => onMonitor([review], monitorEnabled),
                        disabled: !canMutate || !follows.data || busy,
                        separatorBefore: true,
                      },
                      ...(monitorEnabled
                        ? [
                            {
                              label: "Manage monitored threads",
                              run: () => onMonitor([review], false),
                              disabled: !canMutate || busy,
                            },
                          ]
                        : []),
                      ...(snapshot
                        ? [
                            {
                              label: "Follow-up handled",
                              run: () => setAcknowledgement(snapshot),
                              disabled: !canMutate || busy,
                              separatorBefore: true,
                            },
                          ]
                        : []),
                      {
                        label: disposition === "reviewed" ? "✓ Review complete" : "Review complete",
                        run: () => void decide("reviewed"),
                        disabled: !canMutate || busy || (disposition === "reviewed" && !snapshot),
                        separatorBefore: !snapshot,
                      },
                      {
                        label:
                          disposition === "waiting_author"
                            ? "✓ Waiting on author"
                            : "Waiting on author",
                        run: () => void decide("waiting_author"),
                        disabled:
                          !canMutate || busy || (disposition === "waiting_author" && !snapshot),
                      },
                      ...(overview.decision?.reviewedSha
                        ? [
                            {
                              label: "Clear local review record",
                              run: () => void decide("needs_review"),
                              disabled: !canMutate || busy,
                            },
                          ]
                        : []),
                    ]
                  : []),
              ]}
            />
            <button
              type="button"
              className="icon-button"
              aria-label="Close details"
              title="Close details"
              onClick={onClose}
            >
              <Icon name="close" />
            </button>
          </div>
        </div>
        <div className="drawer-heading">
          <h2 id="review-title" title={title}>
            {title}
          </h2>
          <DrawerSummary
            review={review}
            run={run}
            session={session}
            child={Boolean(child)}
            earlierFailure={Boolean(childResult.data?.failure?.followedByLeadReport)}
          />
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
          </div>
        </div>
      </header>
      <div
        id="review-tabpanel"
        key={
          activeTab === "overview"
            ? `${review.reviewId}/${requestedRun ?? ""}/overview`
            : (scrollKey ?? activeTab)
        }
        ref={scrollerRef}
        role="tabpanel"
        aria-labelledby={`review-tab-${activeTab}`}
        className={activeTab === "agent" ? "drawer-body agent-body" : "drawer-body"}
      >
        {outcomeMessage && <Notice>{outcomeMessage}</Notice>}
        {command && (
          <label className="field copy-session-command">
            Exact session command
            <textarea
              value={command}
              readOnly
              rows={2}
              onFocus={(event) => event.currentTarget.select()}
            />
          </label>
        )}
        {activeTab === "agent" && recoveryActive && recovery.data && (
          <Notice>
            Auto-resume {readable(recovery.data.state).toLowerCase()}
            {recovery.data.reason && ` · ${readable(recovery.data.reason)}`}
            {recovery.data.nextAttemptAt && (
              <>
                <br />
                Next automatic retry{" "}
                <time dateTime={recovery.data.nextAttemptAt}>
                  {formatActivityTime(recovery.data.nextAttemptAt)}
                </time>
              </>
            )}
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
              <div ref={setReadingContent} className="drawer-reading-content">
                <ReviewOverviewPane
                  overview={overview}
                  run={run}
                  ticket={ticket}
                  onLaunch={onLaunch}
                  pending={selectionPending}
                  selected={selected}
                />
              </div>
            )}
            {activeTab === "changes" && (
              <Changes key={review.reviewId} reviewId={review.reviewId} webUrl={overview.webUrl}>
                <ChangesMonitor
                  enrollment={follows.data}
                  approved={review.personalApproval}
                  lead={monitorLead ? readable(monitorLead.provider) : undefined}
                  canChange={canMutate && !busy}
                  onEdit={() => onMonitor([review], false)}
                  onCheck={() => void checkMonitoredChanges()}
                />
              </Changes>
            )}
            {activeTab === "agent" &&
              (selected && run && session ? (
                <AgentPanel
                  key={scope}
                  run={run}
                  session={session}
                  reviewTitle={review.title}
                  {...(startedAt ? { startedAt } : {})}
                  state={review.focusSessionKey === run.leadSessionKey ? review.runStage : null}
                  drafts={drafts}
                  onSession={onSession}
                  deliveriesOpen={deliveryScope === scope}
                  leadHasReport={
                    review.focusedRunId === run.runId && Boolean(review.focusedReportReviewedSha)
                  }
                  onTerminal={() => void commandFor("open")}
                />
              ) : selectionPending ? (
                <Loading label="Resolving selected run…" />
              ) : (
                <Empty title="No review run registered">
                  <p>Start or register a review to see its agents.</p>
                </Empty>
              ))}
            {activeTab === "findings" &&
              (selected && child && run ? (
                <div ref={setReadingContent} className="drawer-reading-content">
                  <SessionResult
                    target={{ runId: run.runId, sessionKey: child.sessionKey }}
                    resource={childResult}
                  />
                </div>
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
          confirmLabel={
            acknowledgement.disposition === "reviewed"
              ? "Review complete"
              : acknowledgement.disposition === "waiting_author"
                ? "Waiting on author"
                : "Mark displayed follow-up handled"
          }
          stale={changedSinceCapture}
          busy={busy}
          source={source}
          onClose={() => setAcknowledgement(undefined)}
          onConfirm={() => void confirmAttention()}
          onRefresh={() =>
            setAcknowledgement(
              snapshot && acknowledgement.disposition
                ? { ...snapshot, disposition: acknowledgement.disposition }
                : snapshot,
            )
          }
        />
      )}
    </aside>
  );
  return <DiagramCacheContext value={diagrams}>{drawer}</DiagramCacheContext>;
}

function DrawerSummary({
  review,
  run,
  session,
  child,
  earlierFailure,
}: {
  readonly review: MergeRequestRow;
  readonly run: RunDetail | undefined;
  readonly session: SessionSummary | undefined;
  readonly child: boolean;
  readonly earlierFailure: boolean;
}) {
  const reason =
    !child &&
    run &&
    review.focusedRunId === run.runId &&
    review.focusSessionKey === session?.sessionKey
      ? review.reasonKind
      : null;
  const known: Readonly<Record<string, readonly [string, string]>> = {
    agent_input: ["Awaiting input", "rose"],
    permission_required: ["Agent needs input", "rose"],
    report_ready: ["Response ready", "blue"],
    report_failed: ["Findings could not be saved", "red"],
    lead_working: ["Working", "green"],
    session_working: ["Working", "green"],
    waiting_author: ["Waiting on author", "amber"],
    discussion_update: ["Follow-up ready", "blue"],
    decision_required: ["Review ready", "blue"],
  };
  const native = session?.metadata;
  const archived = reason === "closed_mr" || reason === "archive";
  const state = archived
    ? review.focusedReportReviewedSha
      ? known.report_ready
      : undefined
    : reason
      ? (known[reason] ?? [readable(reason), ""])
      : undefined;
  const nativeState = !state && native?.status;
  if (child) {
    return (
      <div className="drawer-meta">
        {(earlierFailure || nativeState) && (
          <span className="drawer-state badge" title="Last reported native status">
            {earlierFailure ? "Failed earlier" : readable(nativeState || "")}
          </span>
        )}
        <span>Native subagent conversation and result</span>
      </div>
    );
  }
  return (
    <div className="drawer-meta">
      {state && <span className={`drawer-state badge ${state[1]}`}>{state[0]}</span>}
      {nativeState && (
        <span className="drawer-state badge" title="Last reported native status">
          {readable(nativeState)}
        </span>
      )}
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

type AttentionSnapshot = {
  readonly checkpointId: string;
  readonly disposition?: "reviewed" | "waiting_author";
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
    (!attention.source.length &&
      !attention.sessions.length &&
      !attention.reports.length &&
      !attention.followups.length)
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
