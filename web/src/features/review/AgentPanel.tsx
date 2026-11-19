import { useEffect, useRef, useState } from "react";
import { readRun } from "../../api/client";
import type { ReactNode } from "react";
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
  const outbox = useResource(
    `deliveries:${run.runId}:${session.sessionKey}:${cursor ?? ""}:${lead}`,
    (signal) =>
      lead ? messageHistory(target, cursor, signal) : Promise.resolve({ data: null, cursor: "" }),
    [
      { kind: "session", id: session.sessionKey },
      { kind: "run", id: run.runId },
      { kind: "history", id: session.sessionKey },
    ],
  );

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

  const model = lead ? (run.launch?.observedModel ?? run.launch?.requestedModel) : null;
  const effort = lead ? (run.launch?.observedEffort ?? run.launch?.requestedEffort) : null;
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
          <SessionPicker run={run} selected={session} onSession={onSession}>
            <button type="button" className="session-menu-option" onClick={onTerminal}>
              Open in terminal
            </button>
            <ReportedPlan runId={run.runId} sessionKey={session.sessionKey} />
            {lead && (
              <button
                type="button"
                className="session-menu-option"
                aria-expanded={messages}
                onClick={() => setMessages(!messages)}
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
          </SessionPicker>
        </div>
        {notice && <Notice>{notice}</Notice>}
        {messages && lead && (
          <section className="agent-deliveries surface-card stack" aria-label="Delivery history">
            {outbox.error && <Notice retry={outbox.retry}>{outbox.error}</Notice>}
            {outbox.pending && <p className="meta">Loading deliveries…</p>}
            {outbox.data?.items.length === 0 && <p className="meta">No recorded deliveries.</p>}
            {outbox.data?.items.map((item) => (
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
              {outbox.data?.nextCursor && (
                <button onClick={() => setCursor(outbox.data?.nextCursor ?? undefined)}>
                  Older deliveries
                </button>
              )}
            </div>
          </section>
        )}
      </header>
      <Transcript
        target={target}
        messages={cursor ? [] : (outbox.data?.items ?? [])}
        resultOnly={resultOnly}
        working={state === "working"}
        onObservedTurn={setTurn}
      />
      {!resultOnly && (
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
  onSession,
  children,
}: {
  readonly run: RunDetail;
  readonly selected: SessionSummary;
  readonly onSession: Props["onSession"];
  readonly children: ReactNode;
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [open, setOpen] = useState(false);
  const control = useRef<HTMLDetailsElement>(null);
  const cursor = trail.at(-1);
  const page = useResource(`session-page:${run.runId}:${cursor ?? ""}`, async (signal) => {
    if (!cursor) return { data: null, cursor: "" };
    const result = await readRun(run.reviewId, run.runId, signal, cursor);
    if (result.data.leadSessionKey !== run.leadSessionKey)
      throw new Error("The lead session changed.");
    return { data: result.data.sessions, cursor: result.cursor };
  });
  const visible = cursor ? page.data : run.sessions;
  const agents = (visible?.items ?? []).filter((item) => item.sessionKey !== run.leadSessionKey);
  const isLead = selected.sessionKey === run.leadSessionKey;

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
        {isLead ? "Lead agent" : "Subagent"}
        <Icon name="chevron" size={11} />
      </summary>
      {open && (
        <div className="agent-session-options">
          <button
            type="button"
            className="session-menu-option"
            aria-current={isLead || undefined}
            onClick={() => choose(undefined)}
          >
            Lead review
          </button>
          {agents.map((item) => (
            <button
              type="button"
              className="session-menu-option"
              aria-current={item.sessionKey === selected.sessionKey || undefined}
              key={item.sessionKey}
              onClick={() => choose(item.sessionKey)}
            >
              {readable(item.provider)} subagent · {item.nativeId.slice(0, 12)}
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
          <div className="session-menu-actions">{children}</div>
        </div>
      )}
    </details>
  );
}
