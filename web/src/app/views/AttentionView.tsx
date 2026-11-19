import { useEffect, useRef, useState } from "react";
import type { Notification } from "../../api/generated/contracts";
import type { Action } from "../../api/generated/contracts/notifications";
import { assignNotifications, readNotifications } from "../../api/workspace";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, formatActivityTime, readable } from "../../lib/presentation";
import "./attention.css";

export function AttentionView({
  onOpen,
  onChange,
}: {
  readonly onOpen: (item: Notification) => void;
  readonly onChange: () => void;
}) {
  const [mode, setMode] = useState<"unread" | "all">("unread");
  const [older, setOlder] = useState<readonly Notification[]>([]);
  const [next, setNext] = useState<string | null>();
  const [frozen, setFrozen] = useState<readonly string[]>();
  const [eligible, setEligible] = useState<readonly string[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string>();
  const request = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => request.current?.abort(), []);
  const page = useResource(`inbox:${mode}`, (signal) => readNotifications({ view: mode }, signal), [
    { kind: "notification" },
  ]);
  const byId = new Map((page.data?.items ?? []).map((item) => [item.id, item]));
  for (const item of older) if (!byId.has(item.id)) byId.set(item.id, item);
  const groups = group([...byId.values()]);
  const cursor = next === undefined ? page.data?.nextCursor : next;

  function changeMode(nextMode: "unread" | "all") {
    request.current?.abort();
    setMode(nextMode);
    setOlder([]);
    setNext(undefined);
    setFrozen(undefined);
    setEligible([]);
    setNotice(undefined);
  }

  async function more() {
    if (!cursor) return;
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setNotice(undefined);
    try {
      const result = await readNotifications({ view: mode, cursor }, controller.signal);
      if (controller.signal.aborted) return;
      setOlder((previous) => [...previous, ...result.data.items].slice(0, 450));
      setNext(result.data.nextCursor);
    } catch (error) {
      if (!controller.signal.aborted) setNotice(describeError(error));
    } finally {
      if (request.current === controller) setBusy(false);
    }
  }

  async function assign(action: Action, ids: readonly string[]) {
    if (!ids.length || busy) return;
    setBusy(true);
    setNotice(undefined);
    try {
      const { data } = await assignNotifications({ action, ids: [...ids] });
      const affected = new Set(data.affectedIds);
      setOlder((items) =>
        items.flatMap((item) => {
          if (!affected.has(item.id)) return [item];
          if (action === "dismiss" || (action === "read" && mode === "unread")) return [];
          return [{ ...item, readAt: action === "unread" ? null : new Date().toISOString() }];
        }),
      );
      setFrozen((selection) => selection?.filter((id) => !affected.has(id)));
      setEligible((ids) => ids.filter((id) => !affected.has(id)));
      page.retry();
      onChange();
      setNotice(
        `${data.affectedIds.length} ${data.affectedIds.length === 1 ? "update" : "updates"} ${action === "dismiss" ? "dismissed" : `marked ${action}`}.`,
      );
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <div className="inbox-summary">
        <div className="inbox-filters" aria-label="Notification view">
          <button
            type="button"
            aria-pressed={mode === "unread"}
            disabled={busy}
            onClick={() => changeMode("unread")}
          >
            Unread
          </button>
          <button
            type="button"
            aria-pressed={mode === "all"}
            disabled={busy}
            onClick={() => changeMode("all")}
          >
            All
          </button>
        </div>
        <div className="inbox-toolbar">
          {frozen ? (
            <>
              <span>{frozen.length} selected</span>
              <button
                type="button"
                disabled={busy || !frozen.length}
                onClick={() => void assign("read", frozen)}
              >
                Mark read
              </button>
              <button
                type="button"
                disabled={busy || !frozen.length}
                onClick={() => void assign("unread", frozen)}
              >
                Mark unread
              </button>
              <button
                type="button"
                disabled={busy || !frozen.length}
                onClick={() => void assign("dismiss", frozen)}
              >
                Dismiss
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => {
                  setFrozen(undefined);
                  setEligible([]);
                }}
              >
                Cancel
              </button>
            </>
          ) : (
            <>
              {page.data && (
                <span>
                  {mode === "unread" ? page.data.unreadMrCount : groups.length} MRs ·{" "}
                  {mode === "unread" ? page.data.unreadEventCount : byId.size} notifications
                </span>
              )}
              {groups.length > 0 && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setEligible([...byId.keys()]);
                    setFrozen([]);
                  }}
                >
                  Select
                </button>
              )}
            </>
          )}
        </div>
      </div>
      {notice && <Notice>{notice}</Notice>}
      {page.error && <Notice retry={page.retry}>{page.error}</Notice>}
      {page.pending && <Loading label="Loading inbox…" />}
      {page.data && !groups.length && (
        <Empty title={mode === "unread" ? "You’re caught up" : "No notifications yet"} />
      )}
      <ul className="inbox-list">
        {groups.map(([reviewId, items]) => {
          const latest = items[0];
          if (!latest) return null;
          const unread = items.filter((item) => !item.readAt).length;
          const mrUpdates = items.filter((item) => item.source === "gitlab").length;
          const agentUpdates = items.length - mrUpdates;
          const updates = [
            mrUpdates > 0 && `${mrUpdates} MR ${mrUpdates === 1 ? "update" : "updates"}`,
            agentUpdates > 0 &&
              `${agentUpdates} agent ${agentUpdates === 1 ? "update" : "updates"}`,
          ]
            .filter(Boolean)
            .join(" · ");
          const selectable = items
            .filter((item) => eligible.includes(item.id))
            .map((item) => item.id);
          const marked = frozen?.filter((id) => selectable.includes(id)) ?? [];
          return (
            <li
              key={reviewId}
              className="inbox-group"
              data-selected={marked.length > 0 || undefined}
            >
              {frozen && (
                <button
                  className="inbox-select-group"
                  type="button"
                  role="checkbox"
                  aria-checked={
                    marked.length === 0
                      ? false
                      : marked.length === selectable.length
                        ? true
                        : "mixed"
                  }
                  disabled={busy || !selectable.length}
                  onClick={() =>
                    setFrozen(
                      marked.length === selectable.length
                        ? frozen.filter((id) => !selectable.includes(id))
                        : [...new Set([...frozen, ...selectable])],
                    )
                  }
                >
                  <span className="row-checkbox" aria-hidden="true">
                    {marked.length === 0 ? "" : marked.length === selectable.length ? "✓" : "−"}
                  </span>
                  Select MR
                </button>
              )}
              <button type="button" className="inbox-open" onClick={() => onOpen(latest)}>
                <div className="inbox-meta">
                  <span className="inbox-source">{readable(latest.source)}</span>
                  <span className={unread ? "inbox-unread" : "inbox-source"}>
                    {unread ? `${unread} unread` : "Read"}
                  </span>
                </div>
                <strong>{latest.label}</strong>
                <div className="inbox-meta inbox-caption">
                  <span>{updates}</span>
                  <time dateTime={latest.occurredAt}>{formatActivityTime(latest.occurredAt)}</time>
                </div>
              </button>
              <details className="inbox-items">
                <summary>
                  View {items.length} {items.length === 1 ? "update" : "updates"}
                </summary>
                <ul className="inbox-events">
                  {items.map((item) => (
                    <li key={item.id}>
                      {frozen && (
                        <input
                          type="checkbox"
                          aria-label={`Select ${item.label}`}
                          disabled={busy || !eligible.includes(item.id)}
                          checked={frozen.includes(item.id)}
                          onChange={(event) =>
                            setFrozen(
                              event.currentTarget.checked
                                ? [...frozen, item.id]
                                : frozen.filter((id) => id !== item.id),
                            )
                          }
                        />
                      )}
                      <button
                        type="button"
                        className="inbox-event-open"
                        onClick={() => onOpen(item)}
                      >
                        <span>
                          {readable(item.source)} · {readable(item.kind)}
                        </span>
                        <strong>{item.label}</strong>
                        <time dateTime={item.occurredAt}>
                          {formatActivityTime(item.occurredAt)} · {item.readAt ? "Read" : "Unread"}
                        </time>
                      </button>
                      <div className="actions inbox-event-actions">
                        <button
                          className="quiet-button"
                          disabled={busy}
                          type="button"
                          onClick={() => void assign(item.readAt ? "unread" : "read", [item.id])}
                        >
                          {item.readAt ? "Mark unread" : "Mark read"}
                        </button>
                        <button
                          className="quiet-button"
                          disabled={busy}
                          type="button"
                          onClick={() => void assign("dismiss", [item.id])}
                        >
                          Dismiss
                        </button>
                      </div>
                    </li>
                  ))}
                </ul>
              </details>
            </li>
          );
        })}
      </ul>
      {cursor && older.length < 450 && (
        <div className="pagination">
          <button type="button" disabled={busy} onClick={() => void more()}>
            Load older updates
          </button>
        </div>
      )}
    </>
  );
}

function group(items: readonly Notification[]) {
  const result = new Map<string, Notification[]>();
  for (const item of items) {
    const group = result.get(item.reviewId) ?? [];
    group.push(item);
    result.set(item.reviewId, group);
  }
  return [...result];
}
