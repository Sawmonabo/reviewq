import { useEffect, useRef, useState } from "react";
import { loadSprints, readQueue, readQueueSync, refreshQueue } from "../../api/workspace";
import { FilterSelect } from "../../components/FilterSelect";
import { Icon } from "../../components/Icon";
import { Pagination } from "../../components/Pagination";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { ReviewRow, UnmatchedReviewRow } from "../../components/ReviewRow";
import { useResource } from "../../hooks/useResource";
import { describeError, formatActivityTime, formatTime } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import {
  clearedFilters,
  ownerships,
  parseSprint,
  queueParams,
  readFilters,
  sorts,
  states,
} from "../../features/queue/filters";
import type { QueueFilters } from "../../features/queue/filters";
import type { Launch, LaunchTarget, OpenReview } from "../../features/queue/types";
import type { QueueFacets, QueueRow } from "../../api/generated/contracts";

const filterKeys = Object.keys(clearedFilters) as (keyof QueueFilters)[];

type QueueProps = {
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
  const [query, setQuery] = useState(filters.q);
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [checked, setChecked] = useState<readonly string[]>([]);
  const [notice, setNotice] = useState<string>();
  const [refreshing, setRefreshing] = useState(false);
  const [loadingSprint, setLoadingSprint] = useState(false);
  const search = useRef<HTMLInputElement>(null);
  const sprintRequest = useRef(0);
  const [lastFacets, setLastFacets] = useState<QueueFacets>();
  const params = queueParams({ ...filters, q: query }, trail.at(-1));
  const list = useResource(
    `queue:${revision}:${JSON.stringify(params)}`,
    async (signal) => {
      const result = await readQueue(params, signal);
      if (!signal.aborted) setLastFacets(result.data.facets);
      return result;
    },
    [{ kind: "queue" }, { kind: "settings" }],
  );
  const sync = useResource(`sync:${revision}`, readQueueSync, [{ kind: "queue_sync" }]);
  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(filters.q), 180);
    return () => window.clearTimeout(timer);
  }, [filters.q]);
  const page = list.data;
  const facets = page?.facets ?? lastFacets;
  const currentSync = sync.data ?? page?.sync;
  const eligible = (page?.items ?? []).filter(
    (row): row is Extract<QueueRow, { kind: "mr" }> =>
      row.kind === "mr" && row.mrState === "open" && row.queueState !== "archive",
  );
  const captured = eligible.filter((row) => checked.includes(row.reviewId));
  const scopedSprints =
    facets?.sprints.filter((entry) => !filters.boardId || entry.boardId === filters.boardId) ?? [];

  function update(next: QueueFilters) {
    setFilters(next);
    saveValue("queue", next);
    setChecked([]);
    setTrail([]);
  }

  async function chooseSprint(value: string) {
    const request = ++sprintRequest.current;
    const pair = parseSprint(value);
    if (pair && page) {
      setLoadingSprint(true);
      setNotice(undefined);
      try {
        await loadSprints({ expectedRevision: page.settingsRevision, pairs: [pair] });
      } catch (error) {
        if (request === sprintRequest.current) setNotice(describeError(error));
        return;
      } finally {
        if (request === sprintRequest.current) setLoadingSprint(false);
      }
    }
    if (request === sprintRequest.current) update({ ...filters, sprint: value });
  }

  async function refresh() {
    setRefreshing(true);
    setNotice(undefined);
    try {
      const { data } = await refreshQueue();
      if (data.error) setNotice(data.error.message);
      sync.retry();
      if (trail.length) setTrail([]);
      list.retry();
    } catch (error) {
      setNotice(describeError(error));
    } finally {
      setRefreshing(false);
    }
  }

  function clear() {
    update(clearedFilters);
    setQuery("");
    search.current?.focus();
  }

  return (
    <>
      <div className="queue-filters" aria-label="Review filters">
        <FilterSelect
          label="Board"
          scope
          value={filters.boardId}
          options={[
            { value: "", label: "All team boards" },
            ...(facets?.boards ?? []).map(({ id, label }) => ({ value: id, label })),
          ]}
          onChange={(boardId) => {
            sprintRequest.current++;
            setLoadingSprint(false);
            update({ ...filters, boardId, sprint: "" });
          }}
        />
        <FilterSelect
          label="Sprint"
          scope
          value={filters.sprint === "active" ? "" : filters.sprint}
          disabled={loadingSprint}
          options={[
            { value: "", label: "Active sprints" },
            { value: "any", label: "Any sprint" },
            { value: "none", label: "No sprint" },
            ...scopedSprints.map((sprint) => ({
              value: `${sprint.boardId}:${sprint.id}`,
              label: filters.boardId
                ? sprint.label
                : `${sprint.label} · ${facets?.boards.find((board) => board.id === sprint.boardId)?.label ?? sprint.boardId}`,
            })),
          ]}
          onChange={(value) => void chooseSprint(value)}
        />
        <FilterSelect
          label="Repo"
          scope
          value={filters.projectId}
          options={[
            { value: "", label: "All repositories" },
            ...(facets?.projects ?? []).map(({ id, label }) => ({ value: id, label })),
          ]}
          onChange={(projectId) => update({ ...filters, projectId })}
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
        <details className="filter-popover queue-hide">
          <summary className="filter-control">
            Hide
            {(filters.hideReviewed || filters.hideInProgress) && (
              <span className="hide-count">
                {Number(filters.hideReviewed) + Number(filters.hideInProgress)}
              </span>
            )}
            <Icon name="chevron" size={12} />
          </summary>
          <div>
            <label>
              <input
                type="checkbox"
                checked={filters.hideReviewed}
                onChange={(event) =>
                  update({ ...filters, hideReviewed: event.currentTarget.checked })
                }
              />
              Reviewed ({facets?.hideReviewed ?? 0})
            </label>
            <label>
              <input
                type="checkbox"
                checked={filters.hideInProgress}
                onChange={(event) =>
                  update({ ...filters, hideInProgress: event.currentTarget.checked })
                }
              />
              Being reviewed ({facets?.hideInProgress ?? 0})
            </label>
          </div>
        </details>
        <FilterSelect
          label="Sort"
          accessibleLabel="Sort reviews"
          value={filters.sort}
          options={Object.entries(sorts).map(([value, label]) => ({ value, label }))}
          onChange={(value) => update({ ...filters, sort: value as QueueFilters["sort"] })}
        />
        {filterKeys.some((key) => filters[key] !== clearedFilters[key]) && (
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
      </div>
      {loadingSprint && (
        <p className="meta" role="status">
          Loading selected sprint…
        </p>
      )}
      {selecting && (
        <div className="selection-bar actions" aria-label="Selected merge requests">
          <strong>{captured.length} selected</strong>
          <button
            type="button"
            className="primary"
            disabled={!captured.length}
            onClick={() => onLaunch("prepare", captured)}
          >
            Prepare reviews
          </button>
          <button
            type="button"
            disabled={!captured.length}
            onClick={() => onMonitor(captured, false)}
          >
            Auto-review updates
          </button>
          <button
            type="button"
            disabled={!captured.length}
            onClick={() => onMonitor(captured, true)}
          >
            Pause auto-review
          </button>
          <button
            type="button"
            onClick={() => {
              setChecked([]);
              onSelect(false);
            }}
          >
            Cancel
          </button>
        </div>
      )}
      {notice && <Notice>{notice}</Notice>}
      {currentSync?.error && (
        <Notice>
          {currentSync.error.message}
          {currentSync.lastSuccessAt &&
            ` · Last confirmed ${formatTime(currentSync.lastSuccessAt)}`}
        </Notice>
      )}
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
      {page &&
        !page.items.length &&
        (currentSync?.mode !== "connected" ? (
          <Empty
            title={
              currentSync?.mode === "disconnected"
                ? "Source connections paused"
                : "Connect your workspace"
            }
          >
            <p>Configure GitLab, Jira and the team scope to discover reviews.</p>
            <button className="primary" type="button" onClick={onSettings}>
              Open settings
            </button>
          </Empty>
        ) : (
          <Empty
            title={
              filters.q || filters.authorId || filters.hideInProgress || filters.hideReviewed
                ? "No matching reviews"
                : "No reviews in this scope"
            }
          >
            <button type="button" onClick={clear}>
              Clear filters
            </button>
          </Empty>
        ))}
      {page && page.items.length > 0 && (
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
                    {...(selecting && eligible.includes(row)
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
