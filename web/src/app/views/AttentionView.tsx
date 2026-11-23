import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { ApiResponseError } from "../../api/client";
import type { Notification } from "../../api/generated/contracts";
import type { Action, ReviewContext } from "../../api/generated/contracts/notifications";
import { assignNotifications, readNotifications } from "../../api/workspace";
import { Icon } from "../../components/Icon";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { ReviewBadges } from "../../components/ReviewRow";
import { useWorkspaceResource } from "../../hooks/useWorkspaceResource";
import { describeError, formatActivityTime, formatTime, readable } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import "./attention.css";

type InboxHistory = {
  readonly headCursor: string;
  readonly nextCursor: string | null;
  readonly items: readonly Notification[];
  readonly contexts: ReadonlyMap<string, ReviewContext>;
};

type InboxFeedback = { readonly message: string; readonly error?: true };
type SelectedNotification = {
  readonly item: Notification;
  readonly context: ReviewContext | undefined;
};
type InboxFocus = {
  readonly control: HTMLButtonElement;
  readonly group: Element | null;
};

const historyLimit = 450;
const selectionLimit = 100 + historyLimit;
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
  const [selected, setSelected] = useState<ReadonlyMap<string, SelectedNotification>>(
    () => new Map(),
  );
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState<InboxFeedback>();
  const request = useRef<AbortController | undefined>(undefined);
  const activeFilter = useRef<HTMLButtonElement>(null);
  const selectShown = useRef<HTMLInputElement>(null);
  const focusAfterUpdate = useRef<InboxFocus | undefined>(undefined);
  useEffect(() => () => request.current?.abort(), []);
  const page = useWorkspaceResource(
    "Notifications",
    `inbox:${mode}`,
    (signal) => readNotifications({ view: mode }, signal),
    [{ kind: "notification" }, { kind: "review" }, { kind: "queue" }],
    accessLost,
  );
  const headCursor = page.data?.nextCursor;
  // A new head cursor can change the revision, watermark or access scope of every older page.
  const history = older?.headCursor === headCursor ? older : undefined;
  if (!page.data && page.errorCause && older) setOlder(undefined);
  const cursor = history ? history.nextCursor : headCursor;
  const byId = new Map((page.data?.items ?? []).map((item) => [item.id, item]));
  for (const item of history?.items ?? []) if (!byId.has(item.id)) byId.set(item.id, item);
  const contexts = new Map(history?.contexts);
  for (const context of page.data?.reviewContexts ?? []) contexts.set(context.reviewId, context);
  const oldest = history?.items.at(-1) ?? page.data?.items.at(-1);
  let refreshedSelection: Map<string, SelectedNotification> | undefined;
  for (const [id, { item, context }] of selected) {
    const confirmed = byId.get(id);
    if (confirmed) {
      const confirmedContext = contexts.get(confirmed.reviewId);
      if (confirmed !== item || confirmedContext !== context) {
        (refreshedSelection ??= new Map(selected)).set(id, {
          item: confirmed,
          context: confirmedContext,
        });
      }
    } else if (cursor && oldest && compareNotifications(item, oldest) > 0) {
      // A live arrival can displace this choice beyond the refreshed page boundary.
      byId.set(id, item);
      if (context && !contexts.has(item.reviewId)) contexts.set(item.reviewId, context);
    } else {
      (refreshedSelection ??= new Map(selected)).delete(id);
    }
  }
  if (refreshedSelection) setSelected(refreshedSelection);
  const shown = [...byId.values()].sort(compareNotifications);
  const groups = group(shown);
  const unreadIds = shown.filter((item) => !item.readAt).map((item) => item.id);
  const selection = shown.filter((item) => selected.has(item.id));
  const selectedUnread = selection.filter((item) => !item.readAt).map((item) => item.id);
  const selectedRead = selection.filter((item) => item.readAt).map((item) => item.id);
  const allSelected = shown.length > 0 && selection.length === shown.length;
  const staleHistory = Boolean(older && !history && page.data);

  useLayoutEffect(() => {
    const previous = focusAfterUpdate.current;
    if (!previous || busy) return;
    const active = document.activeElement;
    if (active === previous.control) return;
    if (active !== document.body) {
      focusAfterUpdate.current = undefined;
      return;
    }
    const target = previous.control.isConnected
      ? previous.control
      : previous.group?.isConnected
        ? previous.group.querySelector<HTMLElement>("summary")
        : (selectShown.current ?? activeFilter.current);
    target?.focus({ preventScroll: true });
    if (target !== previous.control) focusAfterUpdate.current = undefined;
  });

  function rememberFocus(control: HTMLButtonElement) {
    focusAfterUpdate.current =
      document.activeElement === control
        ? { control, group: control.closest(".inbox-group") }
        : undefined;
  }

  function changeMode(nextMode: "unread" | "all") {
    if (nextMode === mode) return;
    request.current?.abort();
    setMode(nextMode);
    saveValue("inbox.mode", nextMode);
    setOlder(undefined);
    setSelected(new Map());
    setFeedback(undefined);
    focusAfterUpdate.current = undefined;
  }

  function select(ids: readonly string[], checked: boolean) {
    if (!checked) {
      unselect(ids);
      return;
    }
    const next = new Map(selected);
    for (const id of ids) {
      const item = byId.get(id);
      if (item) next.set(id, { item, context: contexts.get(item.reviewId) });
    }
    if (next.size > selectionLimit) {
      setFeedback({ message: `Select up to ${selectionLimit} notifications at once.` });
      return;
    }
    setSelected(next);
  }

  function unselect(ids: readonly string[]) {
    setSelected((previous) => {
      const next = new Map(previous);
      for (const id of ids) next.delete(id);
      return next;
    });
  }

  function refreshHistory() {
    setOlder(undefined);
    page.retry();
  }

  async function more(control: HTMLButtonElement) {
    if (!cursor || !headCursor || busy) return;
    rememberFocus(control);
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setFeedback(undefined);
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
        const headIds = new Set(page.data?.items.map((item) => item.id));
        setSelected((previous) => new Map([...previous].filter(([id]) => headIds.has(id))));
        refreshHistory();
        setFeedback({ message: historyChanged });
      } else {
        if (accessLost(error)) {
          setSelected(new Map());
          refreshHistory();
        }
        setFeedback({ message: describeError(error), error: true });
      }
    } finally {
      if (request.current === controller) setBusy(false);
    }
  }

  async function assign(action: Action, ids: readonly string[], control: HTMLButtonElement) {
    if (!ids.length || busy) return;
    rememberFocus(control);
    setBusy(true);
    setFeedback(undefined);
    try {
      const { data } = await assignNotifications({ action, ids: [...ids] });
      unselect(ids);
      page.retry();
      onChange();
      const count = data.affectedIds.length;
      setFeedback({
        message: count
          ? `${count} ${count === 1 ? "notification" : "notifications"} ${action === "dismiss" ? "dismissed" : `marked ${action}`}.`
          : "No notifications changed.",
      });
    } catch (error) {
      if (accessLost(error)) {
        setSelected(new Map());
        refreshHistory();
      }
      setFeedback({ message: describeError(error), error: true });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="inbox"
      onKeyDown={(event) => {
        if (event.key !== "Escape" || busy || !selection.length) return;
        event.preventDefault();
        setSelected(new Map());
        selectShown.current?.focus({ preventScroll: true });
      }}
    >
      <div className="inbox-toolbar" role="group" aria-label="Notification actions">
        {groups.length > 0 && (
          <SelectionCheckbox
            label="Select all shown notifications"
            inputRef={selectShown}
            checked={allSelected}
            mixed={selection.length > 0 && !allSelected}
            disabled={busy}
            onChange={(checked) => select([...byId.keys()], checked)}
          />
        )}
        <div className="inbox-filters" role="group" aria-label="Filter notifications">
          <button
            type="button"
            ref={mode === "unread" ? activeFilter : undefined}
            aria-pressed={mode === "unread"}
            disabled={busy}
            onClick={() => changeMode("unread")}
          >
            Unread
          </button>
          <button
            type="button"
            ref={mode === "all" ? activeFilter : undefined}
            aria-pressed={mode === "all"}
            disabled={busy}
            onClick={() => changeMode("all")}
          >
            All
          </button>
        </div>
        <div className="inbox-actions">
          {selection.length > 0 ? (
            <>
              <span className="inbox-selection" role="status">
                {quantity(selection.length, "notification")} selected
              </span>
              {selectedUnread.length > 0 && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={(event) => void assign("read", selectedUnread, event.currentTarget)}
                >
                  Mark as read
                </button>
              )}
              {selectedRead.length > 0 && (
                <button
                  type="button"
                  disabled={busy}
                  onClick={(event) => void assign("unread", selectedRead, event.currentTarget)}
                >
                  Mark as unread
                </button>
              )}
              <button
                className="quiet-button"
                type="button"
                disabled={busy}
                onClick={(event) =>
                  void assign(
                    "dismiss",
                    selection.map((item) => item.id),
                    event.currentTarget,
                  )
                }
              >
                Dismiss
              </button>
            </>
          ) : (
            <>
              {page.data && groups.length > 0 && (
                <span className="inbox-total">
                  {quantity(mode === "unread" ? page.data.unreadMrCount : groups.length, "MR")} ·{" "}
                  {quantity(
                    mode === "unread" ? page.data.unreadEventCount : shown.length,
                    "notification",
                  )}
                </span>
              )}
              {unreadIds.length > 0 && (
                <button
                  type="button"
                  className="button-label"
                  disabled={busy}
                  onClick={(event) => void assign("read", unreadIds, event.currentTarget)}
                >
                  <Icon name="check" size={15} />
                  {unreadIds.length === page.data?.unreadEventCount
                    ? "Mark all as read"
                    : "Mark shown as read"}
                </button>
              )}
            </>
          )}
        </div>
      </div>
      {feedback?.error && <Notice>{feedback.message}</Notice>}
      {page.error && <Notice retry={page.retry}>{page.error}</Notice>}
      <p className="inbox-status" role="status">
        {feedback && !feedback.error && feedback.message}
        {staleHistory && feedback?.message !== historyChanged && (
          <span>
            {feedback && !feedback.error ? " " : ""}
            {historyChanged}
          </span>
        )}
      </p>
      {page.pending && <Loading label="Loading inbox…" />}
      {page.data && !groups.length && (
        <Empty title={mode === "unread" ? "You’re caught up" : "No notifications yet"} />
      )}
      <ul className="inbox-list">
        {groups.map(([reviewId, items]) => {
          const latest = items[0];
          if (!latest) return null;
          const context = contexts.get(reviewId);
          const unread = items.filter((item) => !item.readAt).map((item) => item.id);
          const mrUpdates = items.filter((item) => item.source === "gitlab").length;
          const agentUpdates = items.length - mrUpdates;
          const updates = [
            mrUpdates > 0 && `${mrUpdates} MR ${mrUpdates === 1 ? "update" : "updates"}`,
            agentUpdates > 0 &&
              `${agentUpdates} agent ${agentUpdates === 1 ? "update" : "updates"}`,
          ]
            .filter(Boolean)
            .join(" · ");
          const ids = items.map((item) => item.id);
          const marked = items.filter((item) => selected.has(item.id)).length;
          const name = context?.ticketKeys.join(" · ") || context?.title || latest.label;
          const toggleLabel = `Mark ${name} notifications as ${unread.length ? "read" : "unread"}`;
          return (
            <li key={reviewId} className="inbox-group" data-selected={marked > 0 || undefined}>
              <div className="inbox-row">
                <SelectionCheckbox
                  label={`Select ${name} notifications`}
                  checked={marked === ids.length}
                  mixed={marked > 0 && marked < ids.length}
                  disabled={busy}
                  onChange={(checked) => select(ids, checked)}
                />
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
                    <span className={unread.length ? "inbox-unread" : "inbox-source"}>
                      {unread.length ? `${unread.length} unread` : "Read"}
                    </span>
                  </div>
                  <strong>{context?.ticketTitle || context?.title || latest.label}</strong>
                  {context && <span className="inbox-preview">{latest.label}</span>}
                </button>
                <button
                  className="icon-button inbox-toggle"
                  type="button"
                  aria-label={toggleLabel}
                  title={toggleLabel}
                  disabled={busy}
                  onClick={(event) =>
                    void assign(
                      unread.length ? "read" : "unread",
                      unread.length ? unread : ids,
                      event.currentTarget,
                    )
                  }
                >
                  <Icon name={unread.length ? "check" : "inbox"} />
                </button>
              </div>
              <details className="inbox-items">
                <summary>
                  <span>
                    <Icon name="chevronRight" size={13} />
                    {updates}
                  </span>
                  <time dateTime={latest.occurredAt} title={formatTime(latest.occurredAt)}>
                    {formatActivityTime(latest.occurredAt)}
                  </time>
                </summary>
                <ul className="inbox-events">
                  {items.map((item) => (
                    <li key={item.id} data-read={Boolean(item.readAt)}>
                      <SelectionCheckbox
                        label={`Select ${item.label}`}
                        disabled={busy}
                        checked={selected.has(item.id)}
                        onChange={(checked) => select([item.id], checked)}
                      />
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
                          onClick={(event) =>
                            void assign(
                              item.readAt ? "unread" : "read",
                              [item.id],
                              event.currentTarget,
                            )
                          }
                        >
                          {item.readAt ? "Mark as unread" : "Mark as read"}
                        </button>
                        <button
                          className="quiet-button"
                          disabled={busy}
                          type="button"
                          onClick={(event) =>
                            void assign("dismiss", [item.id], event.currentTarget)
                          }
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
          <button type="button" disabled={busy} onClick={(event) => void more(event.currentTarget)}>
            Load older updates
          </button>
        </div>
      )}
    </div>
  );
}

function SelectionCheckbox({
  label,
  checked,
  mixed = false,
  disabled,
  inputRef,
  onChange,
}: {
  readonly label: string;
  readonly checked: boolean;
  readonly mixed?: boolean;
  readonly disabled: boolean;
  readonly inputRef?: RefObject<HTMLInputElement | null>;
  readonly onChange: (checked: boolean) => void;
}) {
  return (
    <label className="inbox-checkbox">
      <input
        type="checkbox"
        aria-label={label}
        checked={checked}
        disabled={disabled}
        ref={(input) => {
          if (input) input.indeterminate = mixed;
          if (inputRef) inputRef.current = input;
        }}
        onChange={(event) => onChange(event.currentTarget.checked)}
      />
    </label>
  );
}

function quantity(count: number, singular: string) {
  return `${count} ${singular}${count === 1 ? "" : "s"}`;
}

function accessLost(error: unknown) {
  return (
    error instanceof ApiResponseError &&
    (error.code === "unauthorized" || error.code === "forbidden" || error.code === "not_found")
  );
}

function compareNotifications(left: Notification, right: Notification) {
  return (
    right.occurredAt.localeCompare(left.occurredAt) ||
    left.source.localeCompare(right.source) ||
    right.id.localeCompare(left.id)
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
