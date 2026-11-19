import { useEffect, useRef, useState } from "react";
import { ApiResponseError } from "../../api/client";
import type { Notification } from "../../api/generated/contracts";
import type { Action, ReviewContext } from "../../api/generated/contracts/notifications";
import { assignNotifications, readNotifications } from "../../api/workspace";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { ReviewBadges } from "../../components/ReviewRow";
import { useResource } from "../../hooks/useResource";
import { describeError, formatActivityTime, formatTime, readable } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import "./attention.css";

type InboxHistory = {
  readonly headCursor: string;
  readonly nextCursor: string | null;
  readonly items: readonly Notification[];
  readonly contexts: ReadonlyMap<string, ReviewContext>;
};

const historyLimit = 450;
const historyChanged = "The inbox changed. Load older updates again to continue.";
const eventLabels: Readonly<Record<string, string>> = {
  author_reply: "Author reply",
  code_changed: "New commits",
  failed: "Agent error",
  finished: "Review response",
  needs_input: "Needs your input",
  new_head: "New commits",
  reopened: "Thread reopened",
  reply: "Thread reply",
  report_ready: "Review response",
  resolved: "Thread resolved",
  thread_reply: "Thread reply",
};

export function AttentionView({
  onOpen,
  onChange,
}: {
  readonly onOpen: (item: Notification) => void;
  readonly onChange: () => void;
}) {
  const [mode, setMode] = useState<"unread" | "all">(() =>
    savedValue("inbox.mode") === "all" ? "all" : "unread",
  );
  const [older, setOlder] = useState<InboxHistory>();
  const [frozen, setFrozen] = useState<readonly string[]>();
  const [eligible, setEligible] = useState<readonly string[]>([]);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string>();
  const request = useRef<AbortController | undefined>(undefined);
  const filterGroup = useRef<HTMLDivElement>(null);
  const selectionTrigger = useRef<HTMLButtonElement>(null);
  const selectShown = useRef<HTMLButtonElement>(null);
  useEffect(() => () => request.current?.abort(), []);
  const page = useResource(`inbox:${mode}`, (signal) => readNotifications({ view: mode }, signal), [
    { kind: "notification" },
    { kind: "review" },
    { kind: "queue" },
  ]);
  const headCursor = page.data?.nextCursor;
  // A new head cursor can change the revision, watermark or access scope of every older page.
  const history = older?.headCursor === headCursor ? older : undefined;
  const byId = new Map((page.data?.items ?? []).map((item) => [item.id, item]));
  for (const item of history?.items ?? []) if (!byId.has(item.id)) byId.set(item.id, item);
  const contexts = new Map(history?.contexts);
  for (const context of page.data?.reviewContexts ?? []) contexts.set(context.reviewId, context);
  const groups = group([...byId.values()]);
  const eligibleShown = eligible.filter((id) => byId.has(id));
  const selected = frozen?.filter((id) => byId.has(id)) ?? [];
  const selectedIds = new Set(selected);
  const selectedGroups = groups.filter(([, items]) =>
    items.some((item) => selectedIds.has(item.id)),
  ).length;
  const allSelected = eligibleShown.length > 0 && selected.length === eligibleShown.length;
  const cursor = history ? history.nextCursor : headCursor;

  function changeMode(nextMode: "unread" | "all") {
    if (nextMode === mode) return;
    request.current?.abort();
    setMode(nextMode);
    saveValue("inbox.mode", nextMode);
    setOlder(undefined);
    setFrozen(undefined);
    setEligible([]);
    setNotice(undefined);
  }

  function leaveSelection() {
    setFrozen(undefined);
    setEligible([]);
    requestAnimationFrame(() => selectionTrigger.current?.focus({ preventScroll: true }));
  }

  function refreshHistory() {
    setOlder(undefined);
    page.retry();
  }

  async function more() {
    if (!cursor || !headCursor || busy) return;
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setNotice(undefined);
    try {
      const result = await readNotifications({ view: mode, cursor }, controller.signal);
      if (controller.signal.aborted) return;
      setOlder((previous) => {
        const retained = previous?.headCursor === headCursor ? previous : undefined;
        const byId = new Map(retained?.items.map((item) => [item.id, item]));
        for (const item of result.data.items) {
          if (!byId.has(item.id)) byId.set(item.id, item);
        }
        const items = [...byId.values()].slice(0, historyLimit);
        const shown = new Set(items.map((item) => item.reviewId));
        const contexts = new Map(retained?.contexts);
        for (const context of result.data.reviewContexts) {
          if (shown.has(context.reviewId)) contexts.set(context.reviewId, context);
        }
        return { headCursor, nextCursor: result.data.nextCursor, items, contexts };
      });
    } catch (error) {
      if (controller.signal.aborted) return;
      if (error instanceof ApiResponseError && error.code === "cursor_expired") {
        refreshHistory();
        setNotice(historyChanged);
      } else {
        setNotice(describeError(error));
      }
    } finally {
      if (request.current === controller) setBusy(false);
    }
  }

  async function assign(action: Action, ids: readonly string[]) {
    if (!ids.length || busy) return;
    const invoker = document.activeElement;
    const group = invoker?.closest(".inbox-items");
    setBusy(true);
    setNotice(undefined);
    try {
      const { data } = await assignNotifications({ action, ids: [...ids] });
      const affected = new Set(data.affectedIds);
      setFrozen((selection) => selection?.filter((id) => !affected.has(id)));
      setEligible((ids) => ids.filter((id) => !affected.has(id)));
      page.retry();
      onChange();
      setNotice(
        `${affected.size} ${affected.size === 1 ? "update" : "updates"} ${action === "dismiss" ? "dismissed" : `marked ${action}`}.${affected.size && history ? ` ${historyChanged}` : ""}`,
      );
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      setBusy(false);
      requestAnimationFrame(() => {
        if (document.activeElement !== document.body) return;
        const activeFilter = filterGroup.current?.querySelector("button[aria-pressed='true']");
        const focus = invoker?.isConnected
          ? invoker
          : group?.isConnected
            ? group.querySelector("summary")
            : activeFilter;
        if (focus instanceof HTMLElement) focus.focus({ preventScroll: true });
      });
    }
  }

  return (
    <>
      <div className="inbox-summary">
        <div
          ref={filterGroup}
          className="inbox-filters"
          role="group"
          aria-label="Filter notifications"
        >
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
          {page.data && (
            <span>
              {mode === "unread" ? page.data.unreadMrCount : groups.length} MRs ·{" "}
              {mode === "unread" ? page.data.unreadEventCount : byId.size} notifications
            </span>
          )}
          {frozen ? (
            <>
              <button
                type="button"
                ref={selectShown}
                className="quiet-button"
                disabled={busy || !eligibleShown.length}
                onClick={() => setFrozen(allSelected ? [] : eligibleShown)}
              >
                {allSelected ? "Clear selection" : "Select all shown"}
              </button>
              <span role="status">{selectedGroups} selected</span>
              <button
                type="button"
                disabled={busy || !selected.length}
                onClick={() => void assign("read", selected)}
              >
                Mark read
              </button>
              <button
                type="button"
                disabled={busy || !selected.length}
                onClick={() => void assign("unread", selected)}
              >
                Mark unread
              </button>
              <button
                type="button"
                disabled={busy || !selected.length}
                onClick={() => void assign("dismiss", selected)}
              >
                Dismiss
              </button>
              <button
                type="button"
                className="quiet-button"
                disabled={busy}
                onClick={leaveSelection}
              >
                Cancel
              </button>
            </>
          ) : (
            <>
              {groups.length > 0 && (
                <button
                  type="button"
                  ref={selectionTrigger}
                  disabled={busy}
                  onClick={() => {
                    setEligible([...byId.keys()]);
                    setFrozen([]);
                    requestAnimationFrame(() =>
                      selectShown.current?.focus({ preventScroll: true }),
                    );
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
      {older && !history && page.data && <Notice>{historyChanged}</Notice>}
      {page.error && <Notice retry={page.retry}>{page.error}</Notice>}
      {page.pending && <Loading label="Loading inbox…" />}
      {page.data && !groups.length && (
        <Empty title={mode === "unread" ? "You’re caught up" : "No notifications yet"} />
      )}
      <ul className="inbox-list">
        {groups.map(([reviewId, items]) => {
          const latest = items[0];
          if (!latest) return null;
          const context = contexts.get(reviewId);
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
                  aria-label={`Select ${context?.ticketKeys.join(" · ") || context?.title || latest.label} notifications`}
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
                  {context ? (
                    <span className="inbox-identity">
                      {context.ticketKeys.length > 0 && (
                        <span className="ticket-key">{context.ticketKeys.join(" · ")}</span>
                      )}
                      <span className="mr-id" title={context.projectPath}>
                        {context.projectPath.split("/").at(-1)} !{context.iid}
                      </span>
                      <ReviewBadges review={context} />
                    </span>
                  ) : (
                    <span className="inbox-source">{notificationSource(latest.source)}</span>
                  )}
                  <span className={unread ? "inbox-unread" : "inbox-source"}>
                    {unread ? `${unread} unread` : "Read"}
                  </span>
                </div>
                <strong>{context?.ticketTitle || context?.title || latest.label}</strong>
                {context && <span className="inbox-preview">{latest.label}</span>}
                <div className="inbox-meta inbox-caption">
                  <span>{updates}</span>
                  <time dateTime={latest.occurredAt} title={formatTime(latest.occurredAt)}>
                    {formatActivityTime(latest.occurredAt)}
                  </time>
                </div>
              </button>
              <details className="inbox-items">
                <summary>
                  View {items.length} {items.length === 1 ? "update" : "updates"}
                </summary>
                <ul className="inbox-events">
                  {items.map((item) => (
                    <li key={item.id} data-read={Boolean(item.readAt)}>
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
                          {notificationSource(item.source)} ·{" "}
                          {eventLabels[item.kind] ?? readable(item.kind)}
                        </span>
                        <strong>{item.label}</strong>
                        <time dateTime={item.occurredAt} title={formatTime(item.occurredAt)}>
                          {formatActivityTime(item.occurredAt)}
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
      {cursor && (history?.items.length ?? 0) < historyLimit && (
        <div className="pagination">
          <button type="button" disabled={busy} onClick={() => void more()}>
            Load older updates
          </button>
        </div>
      )}
    </>
  );
}

function notificationSource(source: string): string {
  if (source === "gitlab") return "GitLab";
  if (source === "native") return "Agent";
  return readable(source);
}

function group(items: readonly Notification[]) {
  const result = new Map<string, Notification[]>();
  for (const item of items) {
    const group = result.get(item.reviewId) ?? [];
    group.push(item);
    result.set(item.reviewId, group);
  }
  return [...result].sort(([leftId, left], [rightId, right]) => {
    const latestLeft = left[0]?.occurredAt ?? "";
    const latestRight = right[0]?.occurredAt ?? "";
    return latestRight.localeCompare(latestLeft) || leftId.localeCompare(rightId);
  });
}
