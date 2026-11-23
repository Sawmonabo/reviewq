import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { readSettings } from "../../api/settings";
import { loadSprints, readQueue, readQueueSync, refreshQueue } from "../../api/workspace";
import { FilterSelect } from "../../components/FilterSelect";
import { Icon } from "../../components/Icon";
import { Pagination } from "../../components/Pagination";
import { Loading, Notice } from "../../components/PanelState";
import { ReviewRow, UnmatchedReviewRow } from "../../components/ReviewRow";
import { useWorkspaceRecovery } from "../../hooks/useWorkspaceRecovery";
import { useWorkspaceResource } from "../../hooks/useWorkspaceResource";
import { describeError, formatActivityTime, formatTime } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import {
  clearedFilters,
  filtersActive,
  ownerships,
  parseSprint,
  queueParams,
  readFilters,
  sorts,
  states,
} from "../../features/queue/filters";
import type { QueueFilters } from "../../features/queue/filters";
import { resolveQueueScope } from "../../features/queue/scope";
import type { Launch, LaunchTarget, OpenReview } from "../../features/queue/types";
import type { QueueFacets, QueueRow } from "../../api/generated/contracts";
import { SelectionActions } from "../SelectionActions";
import { QueueExclusions } from "./QueueExclusions";
import { ScopeFilters } from "./queue-scope/ScopeFilters";

type QueueProps = {
  readonly renderHeader: (selection?: ReactNode) => ReactNode;
  readonly selectedReview?: string | undefined;
  readonly selectedTicket?: string | undefined;
  readonly revision: number;
  readonly selecting: boolean;
  readonly onSelect: (next: boolean) => void;
  readonly onReview: OpenReview;
  readonly onTicket: (key: string) => void;
  readonly onSettings: () => void;
  readonly onLaunch: Launch;
  readonly onMonitor: (targets: readonly LaunchTarget[], pause: boolean) => void;
};

export function QueueView({
  renderHeader,
  selectedReview,
  selectedTicket,
  revision,
  selecting,
  onSelect,
  onReview,
  onTicket,
  onSettings,
  onLaunch,
  onMonitor,
}: QueueProps) {
  const [filters, setFilters] = useState(() => readFilters(savedValue("queue")));
  const latestFilters = useRef(filters);
  const [query, setQuery] = useState(filters.q);
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [checked, setChecked] = useState<readonly string[]>([]);
  const [notice, setNotice] = useState<string>();
  const [refreshing, setRefreshing] = useState(false);
  const [loadingSprint, setLoadingSprint] = useState(false);
  const search = useRef<HTMLInputElement>(null);
  const sprintRequest = useRef(0);
  const [lastFacets, setLastFacets] = useState<QueueFacets>();
  const settingsRead = useWorkspaceResource(
    "Review filters",
    `queue-settings:${revision}`,
    readSettings,
    [{ kind: "settings" }],
  );
  const settings = settingsRead.data;
  const scope = settings ? resolveQueueScope(filters, settings) : undefined;
  const currentFilters = scope?.filters ?? filters;
  const params = queueParams({ ...currentFilters, q: query }, trail.at(-1));
  const list = useWorkspaceResource(
    "Review queue",
    `queue:${revision}:${JSON.stringify(params)}`,
    async (signal) => {
      const baseline =
        params.hideReviewed || params.hideInProgress
          ? readQueue(
              {
                ...params,
                hideReviewed: false,
                hideInProgress: false,
                cursor: undefined,
                limit: 1,
              },
              signal,
            ).catch(() => undefined)
          : undefined;
      const [result, unhidden] = await Promise.all([readQueue(params, signal), baseline]);
      const hiddenCount =
        unhidden?.cursor === result.cursor
          ? Math.max(0, unhidden.data.totalMrs - result.data.totalMrs)
          : undefined;
      if (!signal.aborted) setLastFacets(result.data.facets);
      return { ...result, data: { ...result.data, hiddenCount } };
    },
    [{ kind: "queue" }, { kind: "settings" }],
  );
  const sync = useWorkspaceResource("Queue sync status", `sync:${revision}`, readQueueSync, [
    { kind: "queue_sync" },
  ]);
  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(filters.q), 180);
    return () => window.clearTimeout(timer);
  }, [filters.q]);
  const page = list.data;
  const facets = page?.facets ?? lastFacets;
  const currentSync = sync.data ?? page?.sync;
  useWorkspaceRecovery(
    currentSync?.error
      ? {
          kind: "sync",
          label: "Review sources",
          message: currentSync.error.message,
          confirmedAt: currentSync.lastSuccessAt ?? undefined,
          retry:
            currentSync.mode === "connected" && currentSync.error.retryable
              ? () => void refresh(true)
              : undefined,
        }
      : undefined,
  );
  const eligible = (page?.items ?? []).filter(
    (row): row is Extract<QueueRow, { kind: "mr" }> =>
      row.kind === "mr" && row.mrState === "open" && row.queueState !== "archive",
  );
  const captured = eligible.filter((row) => checked.includes(row.reviewId));

  function update(next: QueueFilters) {
    latestFilters.current = next;
    setFilters(next);
    saveValue("queue", next);
    setChecked([]);
    setTrail([]);
  }

  async function chooseSprint(sprint: QueueFilters["sprint"], sprintPairs: readonly string[]) {
    const request = ++sprintRequest.current;
    const [firstPair, ...remainingPairs] = sprintPairs
      .filter(
        (pair) =>
          currentFilters.sprint !== "selected" || !currentFilters.sprintPairs.includes(pair),
      )
      .flatMap((pair) => parseSprint(pair) ?? []);
    const expectedRevision = settings?.settingsRevision;
    if (firstPair && expectedRevision) {
      setLoadingSprint(true);
      setNotice(undefined);
      try {
        await loadSprints({ expectedRevision, pairs: [firstPair, ...remainingPairs] });
      } catch (error) {
        if (request === sprintRequest.current) setNotice(describeError(error));
        return;
      } finally {
        if (request === sprintRequest.current) setLoadingSprint(false);
      }
    }
    if (request === sprintRequest.current)
      update({ ...latestFilters.current, sprint, sprintPairs });
  }

  async function refresh(keepPage = false) {
    if (refreshing || currentSync?.active) return;
    setRefreshing(true);
    setNotice(undefined);
    try {
      const { data } = await refreshQueue();
      if (data.error) setNotice(data.error.message);
      sync.retry();
      if (!keepPage && trail.length) setTrail([]);
      list.retry();
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      setRefreshing(false);
    }
  }

  function clear() {
    sprintRequest.current++;
    setLoadingSprint(false);
    update(clearedFilters);
    setQuery("");
    search.current?.focus();
  }

  return (
    <>
      {renderHeader(
        selecting ? (
          <SelectionActions
            targets={captured}
            onLaunch={onLaunch}
            onMonitor={onMonitor}
            onCancel={() => {
              setChecked([]);
              onSelect(false);
            }}
          />
        ) : undefined,
      )}
      {currentSync && currentSync.mode !== "connected" && (
        <div className="notice queue-setup" role="status">
          <span>
            {currentSync.mode === "disconnected"
              ? "Review checks are paused. Reconnect in Settings."
              : "Connect GitLab and Jira to choose your team’s reviews."}
          </span>
          <button type="button" onClick={onSettings}>
            Open settings
          </button>
        </div>
      )}
      <div className="queue-filters" aria-label="Review filters">
        <ScopeFilters
          scope={scope}
          settings={settings}
          facets={facets}
          loadingSprint={loadingSprint}
          onChange={(field, ids) => {
            sprintRequest.current++;
            setLoadingSprint(false);
            if (!settings) return;
            const next = { ...latestFilters.current, [field]: ids };
            const normalized = resolveQueueScope(next, settings).filters;
            const missingAllProjects =
              (next.projectIds?.length ?? 0) > 0 && normalized.projectIds?.length === 0;
            update({
              ...normalized,
              projectIds: field === "boardIds" && missingAllProjects ? null : normalized.projectIds,
              sprint:
                next.sprint === "selected" && !normalized.sprintPairs.length
                  ? "active"
                  : next.sprint,
            });
          }}
          onSprint={(mode, pairs) => void chooseSprint(mode, pairs)}
        />
        <FilterSelect
          label="Ownership"
          value={filters.ownership}
          options={Object.entries(ownerships).map(([value, label]) => ({ value, label }))}
          onChange={(value) =>
            update({ ...filters, ownership: value as QueueFilters["ownership"] })
          }
        />
        <FilterSelect
          label="Author"
          value={filters.authorId}
          options={[
            { value: "", label: "Anyone" },
            ...(facets?.authors ?? []).map(({ id, label }) => ({ value: id, label })),
          ]}
          onChange={(authorId) => update({ ...filters, authorId })}
        />
        <FilterSelect
          label="State"
          accessibleLabel="Review state"
          value={filters.state}
          options={Object.entries(states).map(([value, label]) => ({ value, label }))}
          onChange={(value) => update({ ...filters, state: value as QueueFilters["state"] })}
        />
        <QueueExclusions
          reviewed={filters.hideReviewed}
          inProgress={filters.hideInProgress}
          reviewedCount={facets?.hideReviewed}
          inProgressCount={facets?.hideInProgress}
          onReviewed={(hideReviewed) => update({ ...filters, hideReviewed })}
          onInProgress={(hideInProgress) => update({ ...filters, hideInProgress })}
        />
        <FilterSelect
          label="Sort"
          accessibleLabel="Sort reviews"
          value={filters.sort}
          options={Object.entries(sorts).map(([value, label]) => ({ value, label }))}
          onChange={(value) => update({ ...filters, sort: value as QueueFilters["sort"] })}
        />
        {filtersActive(filters) && (
          <button
            type="button"
            className="filter-control clear-filters"
            aria-label="Clear filters"
            title="Clear filters"
            onClick={clear}
          >
            <Icon name="clearFilters" size={17} />
          </button>
        )}
      </div>
      <div className="queue-search-row">
        <input
          className="queue-search"
          ref={search}
          value={filters.q}
          maxLength={200}
          placeholder="Search ticket or title…"
          aria-label="Search reviews or tickets"
          onChange={(event) => update({ ...filters, q: event.currentTarget.value })}
        />
        {page?.hiddenCount !== undefined && (
          <span className="queue-hidden-count" role="status">
            {page.hiddenCount} {page.hiddenCount === 1 ? "MR" : "MRs"} hidden
          </span>
        )}
      </div>
      {loadingSprint && (
        <p className="meta" role="status">
          Loading selected sprint…
        </p>
      )}
      {notice && <Notice>{notice}</Notice>}
      {settingsRead.error && <Notice retry={settingsRead.retry}>{settingsRead.error}</Notice>}
      {list.error && (
        <Notice retry={list.retry}>
          {list.error}
          {trail.length > 0 && (
            <button type="button" onClick={() => setTrail([])}>
              Restart query
            </button>
          )}
        </Notice>
      )}
      {list.pending && <Loading label="Loading review queue…" />}
      {page && (page.items.length > 0 || currentSync?.mode === "connected") && (
        <section className="review-list" aria-label="Reviews">
          <div className="review-list-head">
            {selecting ? (
              <button
                type="button"
                className="select-all-reviews"
                role="checkbox"
                aria-checked={
                  eligible.length > 0 && captured.length === eligible.length
                    ? true
                    : captured.length
                      ? "mixed"
                      : false
                }
                disabled={!eligible.length}
                onClick={() =>
                  setChecked(
                    captured.length === eligible.length ? [] : eligible.map((row) => row.reviewId),
                  )
                }
              >
                <span className="row-checkbox" aria-hidden="true">
                  {captured.length ? (captured.length === eligible.length ? "✓" : "−") : ""}
                </span>
                Select all shown
              </button>
            ) : (
              <span>Review</span>
            )}
            <span>Details</span>
          </div>
          <ul className="review-items">
            {page.items.map((row) =>
              row.kind === "mr" ? (
                <li key={row.reviewId}>
                  <ReviewRow
                    row={row}
                    selected={selectedReview === row.reviewId}
                    onOpen={() => onReview(row.reviewId)}
                    disabled={selecting && !eligible.includes(row)}
                    {...(selecting
                      ? {
                          selection: {
                            checked: checked.includes(row.reviewId),
                            onToggle: () =>
                              setChecked((existing) =>
                                existing.includes(row.reviewId)
                                  ? existing.filter((id) => id !== row.reviewId)
                                  : [...existing, row.reviewId],
                              ),
                          },
                        }
                      : {})}
                  />
                </li>
              ) : (
                <li key={row.key}>
                  <UnmatchedReviewRow
                    row={row}
                    selected={selectedTicket === row.key}
                    disabled={selecting}
                    onOpen={() => onTicket(row.key)}
                  />
                </li>
              ),
            )}
          </ul>
          {!page.items.length && (
            <div className="review-empty" role="status">
              <h3>
                {params.scope === "none"
                  ? "No boards or repositories selected"
                  : "No reviews match these filters"}
              </h3>
              <p>Try changing a filter. New reviews appear automatically.</p>
            </div>
          )}
        </section>
      )}
      <Pagination
        trail={trail}
        next={page?.nextCursor}
        onPage={(next) => {
          setChecked([]);
          setTrail(next);
        }}
      />
      {currentSync && (currentSync.lastSuccessAt || currentSync.mode === "connected") && (
        <footer className="workspace-sync">
          {currentSync.lastSuccessAt && (
            <time
              dateTime={currentSync.lastSuccessAt}
              title={formatTime(currentSync.lastSuccessAt)}
            >
              {currentSync.stale ? "Last successful sync" : "Last updated"}:{" "}
              {formatActivityTime(currentSync.lastSuccessAt)}
            </time>
          )}
          <button
            className="icon-button quiet-button"
            type="button"
            aria-label="Refresh queue"
            title="Refresh queue"
            disabled={refreshing || currentSync.active || currentSync.mode !== "connected"}
            onClick={() => void refresh()}
          >
            <Icon name="refresh" size={13} />
          </button>
        </footer>
      )}
    </>
  );
}
