import { useEffect, useRef, useState } from "react";
import { readRun } from "../../api/client";
import type { RunDetail, RunStage } from "../../api/generated/contracts";
import type { SessionSummary } from "../../api/generated/contracts/hosts";
import { messageHistory } from "../../api/history";
import { interrupt } from "../../api/review";
import { Icon } from "../../components/Icon";
import { Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import type { DraftStore } from "../../lib/drafts";
import { describeError, formatTime, readable } from "../../lib/presentation";
import { Composer } from "./Composer";
import { ReportedPlan } from "./ReportedPlan";
import { NativeQuestions } from "./transcript/NativeQuestions";
import { Transcript } from "./transcript/Transcript";
import "./agent.css";

type Props = {
  readonly run: RunDetail;
  readonly session: SessionSummary;
  readonly reviewTitle: string;
  readonly reviewIdentity: string;
  readonly startedAt?: string | null;
  readonly state: RunStage | null;
  readonly drafts: DraftStore;
  readonly onSession: (key: string | undefined) => void;
  readonly onTerminal: () => void;
  readonly resultOnly?: boolean;
};

export default function AgentPanel({
  run,
  session,
  reviewTitle,
  reviewIdentity,
  startedAt,
  state,
  drafts,
  onSession,
  onTerminal,
  resultOnly = false,
}: Props) {
  const target = { runId: run.runId, sessionKey: session.sessionKey };
  const lead = session.sessionKey === run.leadSessionKey;

  const [notice, setNotice] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [turn, setTurn] = useState<string>();
  const [messages, setMessages] = useState(false);
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
    `earlier-deliveries:${run.runId}:${session.sessionKey}:${cursor ?? ""}`,
    (signal) =>
      lead && cursor
        ? messageHistory(target, cursor, signal)
        : Promise.resolve({ data: null, cursor: "" }),
  );
  const deliveryPage = cursor ? priorDeliveries : outbox;

  async function stop() {
    if (!turn || busy) return;
    setBusy(true);
    setNotice(undefined);
    try {
      const result = await interrupt(run.runId, session.sessionKey, { expectedTurnId: turn });
      setNotice(`Interrupt · ${readable(result.state)}`);
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      setBusy(false);
    }
  }

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
    <div className={`agent-panel ${resultOnly ? "result-panel" : ""}`}>
      <header className="agent-heading">
        <div className="agent-meta">
          <div className="agent-model">
            <strong className="button-label">
              <Icon name="terminal" size={13} />
              {readable(session.provider)}
            </strong>
            <span aria-hidden="true">/</span>
            <span>{model ?? "Model not reported"}</span>
            <span aria-hidden="true">·</span>
            <span>{effort ?? "Effort not reported"}</span>
          </div>
          <div className="agent-tools">
            {(!lead ||
              run.sessions.nextCursor ||
              run.sessions.items.some((item) => item.sessionKey !== run.leadSessionKey)) && (
              <SessionPicker
                run={run}
                selected={session}
                state={state}
                {...(startedAt ? { startedAt } : {})}
                reviewIdentity={reviewIdentity}
                onSession={onSession}
              />
            )}
            <details className="agent-session-menu agent-session-tools">
              <summary className="button-label" aria-label="Session tools">
                <Icon name="more" size={14} />
              </summary>
              <div className="agent-session-options session-menu-actions">
                <ReportedPlan runId={run.runId} sessionKey={session.sessionKey} />
                {lead && (
                  <button
                    type="button"
                    className="session-menu-option"
                    aria-expanded={messages}
                    onClick={(event) => {
                      event.currentTarget.closest("details")?.removeAttribute("open");
                      setMessages(!messages);
                    }}
                  >
                    Delivery history
                  </button>
                )}
                {turn && (
                  <button
                    type="button"
                    className="session-menu-option"
                    disabled={busy}
                    onClick={() => void stop()}
                  >
                    Interrupt session
                  </button>
                )}
              </div>
            </details>
          </div>
        </div>
        {notice && <Notice>{notice}</Notice>}
        {messages && lead && (
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
                    {readable(item.state)} · {readable(item.purpose)}
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
        resultOnly={resultOnly}
        working={lead && state === "working"}
        onObservedTurn={setTurn}
      />
      {!resultOnly && session.provider === "codex" && (
        <NativeQuestions
          target={target}
          store={drafts}
          reviewTitle={reviewTitle}
          onTerminal={onTerminal}
        />
      )}
      {!resultOnly && lead && (
        <Composer
          store={drafts}
          target={target}
          label="Message the review agent…"
          draftLabel={`Message · ${reviewTitle}`}
          targetLabel="To lead review"
          targetTitle={reviewTitle}
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
  startedAt,
  reviewIdentity,
  onSession,
}: {
  readonly run: RunDetail;
  readonly selected: SessionSummary;
  readonly state: RunStage | null;
  readonly startedAt?: string;
  readonly reviewIdentity: string;
  readonly onSession: Props["onSession"];
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [open, setOpen] = useState(false);
  const control = useRef<HTMLDetailsElement>(null);
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
  const leadStatus = state ?? run.leadMetadata.status;
  const leadStarted = run.leadMetadata.startedAt ?? startedAt;
  const count =
    trail.length || visible?.nextCursor
      ? `${agents.length} shown`
      : `${agents.length} in this review`;

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

  function choose(key: string | undefined) {
    if (control.current) control.current.open = false;
    onSession(key);
  }

  return (
    <details
      className="agent-session-menu"
      ref={control}
      onToggle={(event) => setOpen(event.currentTarget.open)}
      onKeyDown={(event) => {
        if (event.key !== "Escape" || !event.currentTarget.open) return;
        event.preventDefault();
        event.stopPropagation();
        event.currentTarget.open = false;
        event.currentTarget.querySelector("summary")?.focus();
      }}
    >
      <summary className="button-label">
        <Icon name="terminal" size={11} />
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
              <strong>{run.leadMetadata.name ?? "Lead review"}</strong>
              <small>
                {reviewIdentity}
                {leadStarted && ` · ${formatTime(leadStarted)}`}
              </small>
            </span>
            {leadStatus && (
              <small className={`session-menu-status ${leadStatus}`}>
                {leadStatus === "needs_input" ? "Awaiting input" : readable(leadStatus)}
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
                    ? formatTime(item.metadata.startedAt)
                    : item.nativeId.slice(0, 24)}
                </small>
              </span>
              {item.metadata.status && (
                <small
                  className={`session-menu-status ${item.metadata.status}`}
                  title="Last reported native status"
                >
                  {readable(item.metadata.status)}
                </small>
              )}
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
        </div>
      )}
    </details>
  );
}
