import { useEffect, useRef, useState } from "react";
import type { Notification } from "../../api/generated/contracts";
import type { Action } from "../../api/generated/contracts/notifications";
import { assignNotifications, readNotifications } from "../../api/workspace";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, formatTime, readable } from "../../lib/presentation";

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
      <div className="filter-stack">
        <div className="actions" aria-label="Inbox view">
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
            All updates
          </button>
          <span className="meta">
            {page.data?.unreadEventCount ?? "—"} unread updates · {page.data?.unreadMrCount ?? "—"}{" "}
            reviews
          </span>
        </div>
      </div>
      {notice && <Notice>{notice}</Notice>}
      {page.error && <Notice retry={page.retry}>{page.error}</Notice>}
      {page.pending && <Loading label="Loading inbox…" />}
      {page.data && !groups.length && (
        <Empty title={mode === "unread" ? "You’re caught up" : "No notifications yet"} />
      )}
      {groups.length > 0 && (
        <div className="selection-bar actions">
          {frozen ? (
            <>
              <strong>{frozen.length} selected</strong>
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
            <button
              type="button"
              onClick={() => {
                const displayed = [...byId.keys()];
                setEligible(displayed);
                setFrozen(displayed);
              }}
            >
              Select displayed updates
            </button>
          )}
        </div>
      )}
      <ul className="inbox-list">
        {groups.map(([reviewId, items]) => {
          const latest = items[0];
          if (!latest) return null;
          const unread = items.filter((item) => !item.readAt).length;
          return (
            <li key={reviewId} className="surface-card inbox-group">
              <details>
                <summary>
                  <span
                    className={`inbox-dot ${unread ? "unread" : ""}`}
                    aria-label={unread ? "Unread" : "Read"}
                  />
                  <strong>{latest.label}</strong>
                  <span className="meta">
                    {items.length} {items.length === 1 ? "update" : "updates"} ·{" "}
                    <time dateTime={latest.occurredAt}>{formatTime(latest.occurredAt)}</time>
                  </span>
                </summary>
                <ul className="inbox-events">
                  {items.map((item) => (
                    <li key={item.id}>
                      <div className="actions">
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
                        <button type="button" className="quiet-button" onClick={() => onOpen(item)}>
                          {item.label}
                        </button>
                        <span className="badge">{readable(item.source)}</span>
                      </div>
                      <span className="meta">
                        <time dateTime={item.occurredAt}>{formatTime(item.occurredAt)}</time> ·{" "}
                        {item.readAt ? "Read" : "Unread"}
                      </span>
                      <div className="actions">
                        <button
                          disabled={busy}
                          type="button"
                          onClick={() => void assign(item.readAt ? "unread" : "read", [item.id])}
                        >
                          {item.readAt ? "Mark unread" : "Mark read"}
                        </button>
                        <button
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
