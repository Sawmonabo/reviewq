import { useState } from "react";
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

  return (
    <div className={`agent-panel ${resultOnly ? "result-panel" : ""}`}>
      <header className="agent-heading stack">
        <div className="spread">
          <div className="actions">
            <span className="agent-mark">
              <Icon name="terminal" size={16} />
            </span>
            <strong>
              {readable(session.provider)} {lead ? "lead" : "child"}
            </strong>
            {state && lead && (
              <span
                className={`badge ${state === "working" ? "green" : state === "needs_input" ? "rose" : ""}`}
              >
                {readable(state)}
              </span>
            )}
          </div>
          <div className="actions">
            <SessionPicker run={run} selected={session} onSession={onSession} />
            <button
              type="button"
              aria-label="Open selected session in terminal"
              title="Open in terminal"
              className="icon-button"
              onClick={onTerminal}
            >
              <Icon name="external" size={14} />
            </button>
          </div>
        </div>
        <div className="actions meta">
          <ReportedPlan runId={run.runId} sessionKey={session.sessionKey} />
          {lead && (
            <button
              type="button"
              className="quiet-button"
              aria-expanded={messages}
              onClick={() => setMessages(!messages)}
            >
              Delivery history
            </button>
          )}
          {turn && (
            <button
              type="button"
              disabled={busy}
              className="quiet-button"
              onClick={() => void stop()}
            >
              Interrupt
            </button>
          )}
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
          label="Message the lead"
          draftLabel={`Message · ${reviewTitle}`}
          targetLabel={`To ${readable(session.provider)} lead · ${session.nativeId.slice(0, 12)}`}
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
}: {
  readonly run: RunDetail;
  readonly selected: SessionSummary;
  readonly onSession: Props["onSession"];
}) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const cursor = trail.at(-1);
  const page = useResource(`session-page:${run.runId}:${cursor ?? ""}`, async (signal) => {
    if (!cursor) return { data: null, cursor: "" };
    const result = await readRun(run.reviewId, run.runId, signal, cursor);
    if (result.data.leadSessionKey !== run.leadSessionKey)
      throw new Error("The lead session changed.");
    return { data: result.data.sessions, cursor: result.cursor };
  });
  const visible = cursor ? page.data : run.sessions;
  const children = (visible?.items ?? []).filter((item) => item.sessionKey !== run.leadSessionKey);
  const selectedChild = selected.sessionKey !== run.leadSessionKey;
  const chooser =
    selectedChild || children.length > 0 || Boolean(visible?.nextCursor) || trail.length > 0;
  if (!chooser) return null;

  return (
    <div className="actions">
      <select
        aria-label="Choose agent session"
        value={selectedChild ? selected.sessionKey : ""}
        onChange={(event) => onSession(event.currentTarget.value || undefined)}
      >
        <option value="">Lead conversation</option>
        {selectedChild && !children.some((item) => item.sessionKey === selected.sessionKey) && (
          <option value={selected.sessionKey}>
            {readable(selected.provider)} child · {selected.nativeId.slice(0, 12)}
          </option>
        )}
        {children.map((item) => (
          <option key={item.sessionKey} value={item.sessionKey}>
            {readable(item.provider)} child · {item.nativeId.slice(0, 12)}
          </option>
        ))}
      </select>
      {trail.length > 0 && (
        <button type="button" onClick={() => setTrail(trail.slice(0, -1))}>
          Previous agents
        </button>
      )}
      {visible?.nextCursor && (
        <button
          type="button"
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
  );
}
