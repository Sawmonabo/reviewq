import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { readRun } from "../../api/client";
import type { RunDetail, RunStage } from "../../api/generated/contracts";
import type { SessionSummary } from "../../api/generated/contracts/hosts";
import { messageHistory } from "../../api/history";
import { Icon } from "../../components/Icon";
import { Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import type { DraftStore } from "../../lib/drafts";
import { formatActivityTime, formatTime, readable } from "../../lib/presentation";
import { deliveryLabels } from "./delivery-labels";
import { Composer } from "./Composer";
import { ReportedPlan } from "./ReportedPlan";
import { NativeQuestions } from "./transcript/NativeQuestions";
import { ProviderMark } from "./transcript/ProviderMark";
import { Transcript } from "./transcript/Transcript";
import "./agent.css";

type Props = {
  readonly run: RunDetail;
  readonly session: SessionSummary;
  readonly reviewTitle: string;
  readonly startedAt?: string | null;
  readonly state: RunStage | null;
  readonly drafts: DraftStore;
  readonly onSession: (key: string | undefined) => void;
  readonly onTerminal: () => void;
  readonly deliveriesOpen: boolean;
  readonly leadHasReport: boolean;
};

export default function AgentPanel({
  run,
  session,
  reviewTitle,
  startedAt,
  state,
  drafts,
  onSession,
  onTerminal,
  deliveriesOpen,
  leadHasReport,
}: Props) {
  const target = { runId: run.runId, sessionKey: session.sessionKey };
  const lead = session.sessionKey === run.leadSessionKey;

  const [cursor, setCursor] = useState<string>();
  const deliveryKeys = [
    { kind: "session", id: session.sessionKey },
    { kind: "run", id: run.runId },
    { kind: "history", id: session.sessionKey },
  ] as const;
  const outbox = useResource(
    `deliveries:${run.runId}:${session.sessionKey}:${lead}`,
    (signal) =>
      lead
        ? messageHistory(target, undefined, signal)
        : Promise.resolve({ data: null, cursor: "" }),
    deliveryKeys,
  );
  const priorDeliveries = useResource(
    `earlier-deliveries:${run.runId}:${session.sessionKey}:${deliveriesOpen ? (cursor ?? "") : ""}`,
    (signal) =>
      lead && deliveriesOpen && cursor
        ? messageHistory(target, cursor, signal)
        : Promise.resolve({ data: null, cursor: "" }),
  );
  const deliveryPage = cursor ? priorDeliveries : outbox;

  const model =
    session.metadata.model ??
    (lead
      ? (run.launch?.observedModel ??
        (run.launch?.requestedModel && `Requested: ${run.launch.requestedModel}`))
      : null);
  const effort =
    session.metadata.effort ??
    (lead
      ? (run.launch?.observedEffort ??
        (run.launch?.requestedEffort && `Requested: ${run.launch.requestedEffort}`))
      : null);
  return (
    <div className="agent-panel">
      <header className="agent-heading">
        <div className="agent-meta">
          <div className="agent-model">
            <strong className="button-label">
              <ProviderMark provider={session.provider} />
              {readable(session.provider)}
            </strong>
            <span aria-hidden="true">/</span>
            <span>{model ?? "Model not reported"}</span>
            <span aria-hidden="true">·</span>
            <span>{effort ?? "Effort not reported"}</span>
          </div>
          {(!lead ||
            run.sessions.nextCursor ||
            run.sessions.items.some((item) => item.sessionKey !== run.leadSessionKey)) && (
            <SessionPicker
              run={run}
              selected={session}
              state={state}
              leadHasReport={leadHasReport}
              {...(startedAt ? { startedAt } : {})}
              onSession={onSession}
            />
          )}
        </div>
        {deliveriesOpen && lead && (
          <section className="agent-deliveries surface-card stack" aria-label="Delivery history">
            {deliveryPage.error && <Notice retry={deliveryPage.retry}>{deliveryPage.error}</Notice>}
            {deliveryPage.pending && <p className="meta">Loading deliveries…</p>}
            {deliveryPage.data?.items.length === 0 && (
              <p className="meta">No recorded deliveries.</p>
            )}
            {deliveryPage.data?.items.map((item) => (
              <div className="stack" key={item.messageId}>
                <div className="spread">
                  <span className="badge">
                    {deliveryLabels[item.state]} · {readable(item.purpose)}
                  </span>
                  <time className="meta" dateTime={item.updatedAt}>
                    {formatTime(item.updatedAt)}
                  </time>
                </div>
                <p className="message-copy">{item.text}</p>
                {item.attachment?.receiptId && (
                  <p className="meta">
                    Report version · <code>{item.attachment.receiptId}</code>
                  </p>
                )}
                {item.outcomeDetail && <p className="meta">{item.outcomeDetail}</p>}
              </div>
            ))}
            <div className="actions">
              {cursor && <button onClick={() => setCursor(undefined)}>Latest deliveries</button>}
              {deliveryPage.data?.nextCursor && (
                <button onClick={() => setCursor(deliveryPage.data?.nextCursor ?? undefined)}>
                  Older deliveries
                </button>
              )}
            </div>
          </section>
        )}
      </header>
      <Transcript
        target={target}
        provider={session.provider}
        child={!lead}
        messages={outbox.data?.items ?? []}
        working={
          ["working", "running"].includes(session.metadata.status ?? "") ||
          (lead && !session.metadata.status && state === "working")
        }
      />
      {session.provider === "codex" && (
        <NativeQuestions
          target={target}
          store={drafts}
          reviewTitle={reviewTitle}
          sessionName={session.metadata.name ?? (lead ? "lead review" : "subagent")}
          child={!lead}
          onTerminal={onTerminal}
        />
      )}
      {lead && (
        <Composer
          store={drafts}
          target={target}
          label="Follow-up message"
          placeholder="Message the review agent…"
          draftLabel={`Message · ${reviewTitle}`}
          targetLabel="To lead review"
          targetTitle={`To lead review${session.metadata.name ? ` · ${session.metadata.name}` : ""}`}
          draftKey={JSON.stringify(["message", run.reviewId, run.runId, run.leadSessionKey])}
          onAccepted={outbox.retry}
        />
      )}
    </div>
  );
}

function SessionPicker({
  run,
  selected,
  state,
  leadHasReport,
  startedAt,
  onSession,
}: {
  readonly run: RunDetail;
  readonly selected: SessionSummary;
  readonly leadHasReport: boolean;
  readonly state: RunStage | null;
  readonly startedAt?: string;
  readonly onSession: Props["onSession"];
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const { open, control: menuRef, close, keydown, sync } = useSessionMenu();
  const cursor = trail.at(-1);
  const page = useResource(
    `session-page:${run.runId}:${cursor ?? ""}`,
    async (signal) => {
      if (!cursor) return { data: null, cursor: "" };
      const result = await readRun(run.reviewId, run.runId, signal, cursor);
      if (result.data.leadSessionKey !== run.leadSessionKey)
        throw new Error("The lead session changed.");
      return { data: result.data.sessions, cursor: result.cursor };
    },
    open && cursor ? [{ kind: "run", id: run.runId }] : [],
  );
  const visible = cursor ? page.data : run.sessions;
  const agents = (visible?.items ?? []).filter((item) => item.sessionKey !== run.leadSessionKey);
  const isLead = selected.sessionKey === run.leadSessionKey;
  const nativeStatus = run.leadMetadata.status;
  const leadStatus =
    leadHasReport && ["idle", "stopped", "completed"].includes(nativeStatus ?? "")
      ? "report_ready"
      : (nativeStatus ?? (state === "working" ? "working" : "unknown"));
  const leadStarted = run.leadMetadata.startedAt ?? startedAt;
  const count =
    trail.length || visible?.nextCursor
      ? `${agents.length} shown`
      : `${agents.length} in this review`;

  function choose(key: string | undefined) {
    close(true);
    if ((key ?? run.leadSessionKey) !== selected.sessionKey) onSession(key);
  }

  return (
    <details className="agent-session-menu" ref={menuRef} onToggle={sync} onKeyDown={keydown}>
      <summary
        className="button-label"
        aria-label={`Subagents · ${count}. Viewing ${isLead ? "lead agent" : (selected.metadata.name ?? "subagent")}`}
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.65"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <rect x="5" y="7" width="14" height="13" rx="3" />
          <path d="M12 3v4M3 12v4m18-4v4M9 16h6M9 11h.01M15 11h.01" />
        </svg>
        <span className="agent-session-selection" title={selected.metadata.name ?? undefined}>
          {isLead ? "Lead agent" : (selected.metadata.name ?? "Subagent")}
        </span>
        <Icon name="chevron" size={11} />
      </summary>
      {open && (
        <div className="agent-session-options">
          <button
            type="button"
            className="session-menu-option session-menu-lead"
            aria-current={isLead || undefined}
            onClick={() => choose(undefined)}
          >
            <span className="session-menu-name">
              <strong>Lead review</strong>
              <small>
                {run.leadMetadata.name ?? (leadStarted && formatActivityTime(leadStarted))}
              </small>
            </span>
            {leadStatus && (
              <small className={`session-menu-status ${leadStatus}`}>
                {sessionStatus(leadStatus)}
              </small>
            )}
          </button>
          <div className="session-menu-divider">
            <span>Subagents</span>
            <small>{count}</small>
          </div>
          {agents.map((item) => (
            <button
              type="button"
              className="session-menu-option"
              aria-current={item.sessionKey === selected.sessionKey || undefined}
              key={item.sessionKey}
              onClick={() => choose(item.sessionKey)}
            >
              <span className="session-menu-name">
                <strong>{item.metadata.name ?? `${readable(item.provider)} subagent`}</strong>
                <small title={item.nativeId}>
                  {item.metadata.startedAt
                    ? `${readable(item.provider)} · ${formatActivityTime(item.metadata.startedAt)}`
                    : readable(item.provider)}
                </small>
              </span>
              {item.metadata.status && (
                <small
                  className={`session-menu-status ${item.metadata.status}`}
                  title="Last reported native status"
                >
                  {sessionStatus(item.metadata.status)}
                </small>
              )}
              <span aria-hidden="true">›</span>
            </button>
          ))}
          {trail.length > 0 && (
            <button
              type="button"
              className="session-menu-option"
              onClick={() => setTrail(trail.slice(0, -1))}
            >
              Previous agents
            </button>
          )}
          {visible?.nextCursor && (
            <button
              type="button"
              className="session-menu-option"
              onClick={() => {
                if (visible.nextCursor) setTrail([...trail, visible.nextCursor]);
              }}
            >
              More agents
            </button>
          )}
          {cursor && page.pending && (
            <span className="meta" role="status">
              Loading sessions…
            </span>
          )}
          {cursor && page.error && <Notice retry={page.retry}>{page.error}</Notice>}
          {!isLead && <ReportedPlan runId={run.runId} sessionKey={selected.sessionKey} />}
        </div>
      )}
    </details>
  );
}

function sessionStatus(status: string) {
  const labels: Readonly<Record<string, string>> = {
    needs_input: "Awaiting input",
    not_loaded: "Not available",
    unknown: "Not available",
    report_ready: "Response ready",
    system_error: "Failed",
    running: "Working",
  };
  return labels[status] ?? readable(status);
}

function useSessionMenu() {
  const [open, setOpen] = useState(false);
  const control = useRef<HTMLDetailsElement>(null);

  useEffect(() => {
    if (!open) return;
    function dismiss(event: PointerEvent) {
      if (
        event.target instanceof Node &&
        control.current &&
        !control.current.contains(event.target)
      )
        control.current.open = false;
    }
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);

  function close(restoreFocus: boolean) {
    if (!control.current) return;
    control.current.open = false;
    if (restoreFocus) control.current.querySelector("summary")?.focus({ preventScroll: true });
  }

  function keydown(event: KeyboardEvent<HTMLDetailsElement>) {
    if (event.key !== "Escape" || !event.currentTarget.open) return;
    event.preventDefault();
    event.stopPropagation();
    close(true);
  }

  return { open, control, close, keydown, sync: () => setOpen(Boolean(control.current?.open)) };
}
