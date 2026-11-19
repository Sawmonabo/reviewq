import { useEffect, useRef, useState } from "react";
import { loadSprints, readQueue, readQueueSync, refreshQueue } from "../../api/workspace";
import { Icon } from "../../components/Icon";
import { Pagination } from "../../components/Pagination";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { ReviewRow } from "../../components/ReviewRow";
import { useResource } from "../../hooks/useResource";
import { describeError, formatTime } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import { clearedFilters, ownerships, parseSprint, queueParams, readFilters, sorts, states } from "../../features/queue/filters";
import type { QueueFilters } from "../../features/queue/filters";
import type { Launch, LaunchTarget, OpenReview } from "../../features/queue/types";
import type { QueueRow } from "../../api/generated/contracts";

type QueueProps = {
  readonly selectedReview?: string | undefined;
  readonly revision: number;
  readonly selecting: boolean;
  readonly onSelect: (next: boolean) => void;
  readonly onReview: OpenReview;
  readonly onTicket: (key: string) => void;
  readonly onSettings: () => void;
  readonly onLaunch: Launch;
  readonly onMonitor: (targets: readonly LaunchTarget[], pause: boolean) => void;
};

export function QueueView({ selectedReview, revision, selecting, onSelect, onReview, onTicket, onSettings, onLaunch, onMonitor }: QueueProps) {
  const [filters, setFilters] = useState(() => readFilters(savedValue("queue")));
  const [query, setQuery] = useState(filters.q);
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [checked, setChecked] = useState<readonly string[]>([]);
  const [notice, setNotice] = useState<string>();
  const [refreshing, setRefreshing] = useState(false);
  const [loadingSprint, setLoadingSprint] = useState(false);
  const search = useRef<HTMLInputElement>(null);
  const sprintRequest = useRef(0);
  const [lastFacets, setLastFacets] = useState<import("../../api/generated/contracts").QueueFacets>();
  const params = queueParams({ ...filters, q: query }, trail.at(-1));
  const list = useResource(`queue:${revision}:${JSON.stringify(params)}`, async (signal) => {
    const result = await readQueue(params, signal);
    if (!signal.aborted) setLastFacets(result.data.facets);
    return result;
  }, [{ kind: "queue" }, { kind: "settings" }]);
  const sync = useResource(`sync:${revision}`, readQueueSync, [{ kind: "queue_sync" }]);
  useEffect(() => {
    const timer = window.setTimeout(() => setQuery(filters.q), 180);
    return () => window.clearTimeout(timer);
  }, [filters.q]);
  const page = list.data;
  const facets = page?.facets ?? lastFacets;
  const currentSync = sync.data ?? page?.sync;
  const eligible = (page?.items ?? []).filter((row): row is Extract<QueueRow, { kind: "mr" }> =>
    row.kind === "mr" && row.mrState === "open" && row.queueState !== "archive");
  const captured = eligible.filter((row) => checked.includes(row.reviewId));
  const scopedSprints = facets?.sprints.filter((entry) => !filters.boardId || entry.boardId === filters.boardId) ?? [];

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

  return <>
    <div className="filter-stack">
      <div className="filter-bar" aria-label="Review scope">
        <label>Board<select value={filters.boardId} onChange={(event) => {
          sprintRequest.current++;
          setLoadingSprint(false);
          update({ ...filters, boardId: event.currentTarget.value, sprint: "" });
        }}><option value="">All boards</option>{facets?.boards.map((board) => <option key={board.id} value={board.id}>{board.label}</option>)}</select></label>
        <label>Sprint<select value={filters.sprint} disabled={loadingSprint} onChange={(event) => void chooseSprint(event.currentTarget.value)}>
          <option value="">Default sprint scope</option><option value="active">Active sprints</option><option value="any">Any sprint</option><option value="none">No sprint</option>
          {scopedSprints.map((sprint) => <option key={`${sprint.boardId}:${sprint.id}`} value={`${sprint.boardId}:${sprint.id}`}>{sprint.label}{!filters.boardId ? ` · ${facets?.boards.find((board) => board.id === sprint.boardId)?.label ?? sprint.boardId}` : ""}</option>)}
        </select></label>
        <label>Repo<select value={filters.projectId} onChange={(event) => update({ ...filters, projectId: event.currentTarget.value })}><option value="">All repositories</option>{facets?.projects.map((project) => <option key={project.id} value={project.id}>{project.label}</option>)}</select></label>
      </div>
      <div className="filter-bar">
        <label>Ownership<select value={filters.ownership} onChange={(event) => update({ ...filters, ownership: event.currentTarget.value as QueueFilters["ownership"] })}>{Object.entries(ownerships).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
        <label>Author<select value={filters.authorId} onChange={(event) => update({ ...filters, authorId: event.currentTarget.value })}><option value="">Anyone</option>{facets?.authors.map((author) => <option key={author.id} value={author.id}>{author.label}</option>)}</select></label>
        <label>State<select value={filters.state} onChange={(event) => update({ ...filters, state: event.currentTarget.value as QueueFilters["state"] })}>{Object.entries(states).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
        <label>Sort<select value={filters.sort} onChange={(event) => update({ ...filters, sort: event.currentTarget.value as QueueFilters["sort"] })}>{Object.entries(sorts).map(([value, label]) => <option key={value} value={value}>{label}</option>)}</select></label>
        <details className="filter-popover"><summary>Hide <Icon name="chevron" size={12} /></summary><div>
          <label><input type="checkbox" checked={filters.hideReviewed} onChange={(event) => update({ ...filters, hideReviewed: event.currentTarget.checked })} /> Reviewed ({facets?.hideReviewed ?? 0})</label>
          <label><input type="checkbox" checked={filters.hideInProgress} onChange={(event) => update({ ...filters, hideInProgress: event.currentTarget.checked })} /> Being reviewed ({facets?.hideInProgress ?? 0})</label>
        </div></details>
        <button type="button" className="quiet-button" onClick={clear}>Clear filters</button>
      </div>
      <label className="search-field"><Icon name="search" /><input ref={search} value={filters.q} maxLength={200} placeholder="Search reviews or tickets" aria-label="Search reviews or tickets" onChange={(event) => update({ ...filters, q: event.currentTarget.value })} /></label>
    </div>
    <div className="list-heading spread">
      <span>{page ? <><strong>{page.totalRows}</strong> {page.totalRows === 1 ? "result" : "results"} <span className="muted">· {page.totalMrs} MRs</span></> : "Queue results"}</span>
      <div className="actions">
        {currentSync?.lastSuccessAt && <span className="meta" title={formatTime(currentSync.lastSuccessAt)}>{currentSync.stale ? "Last successful sync" : "Synced"} · {formatTime(currentSync.lastSuccessAt)}</span>}
        <button className="icon-button" type="button" aria-label="Refresh queue" disabled={refreshing || currentSync?.active || currentSync?.mode !== "connected"} onClick={() => void refresh()}><Icon name="refresh" /></button>
      </div>
    </div>
    {selecting && <div className="selection-bar actions" aria-label="Selected merge requests">
      <strong>{captured.length} selected</strong><button type="button" disabled={!eligible.length} onClick={() => setChecked(eligible.map((row) => row.reviewId))}>Select all shown</button>
      <button type="button" className="primary" disabled={!captured.length} onClick={() => onLaunch("prepare", captured)}>Prepare reviews</button>
      <button type="button" disabled={!captured.length} onClick={() => onMonitor(captured, false)}>Auto-review updates</button>
      <button type="button" disabled={!captured.length} onClick={() => onMonitor(captured, true)}>Pause auto-review</button>
      <button type="button" onClick={() => { setChecked([]); onSelect(false); }}>Cancel</button>
    </div>}
    {notice && <Notice>{notice}</Notice>}
    {currentSync?.error && <Notice>{currentSync.error.message}{currentSync.lastSuccessAt && ` · Last confirmed ${formatTime(currentSync.lastSuccessAt)}`}</Notice>}
    {list.error && <Notice retry={list.retry}>{list.error}{trail.length > 0 && <button type="button" onClick={() => setTrail([])}>Restart query</button>}</Notice>}
    {list.pending && <Loading label="Loading review queue…" />}
    {page && !page.items.length && (currentSync?.mode !== "connected"
      ? <Empty title={currentSync?.mode === "disconnected" ? "Source connections paused" : "Connect your workspace"}><p>Configure GitLab, Jira and the team scope to discover reviews.</p><button className="primary" type="button" onClick={onSettings}>Open settings</button></Empty>
      : <Empty title={filters.q || filters.authorId || filters.hideInProgress || filters.hideReviewed ? "No matching reviews" : "No reviews in this scope"}><button type="button" onClick={clear}>Clear filters</button></Empty>)}
    {page && page.items.length > 0 && <ul className="review-list">
      {page.items.map((row) => row.kind === "mr"
        ? <li key={row.reviewId}><ReviewRow row={row} selected={selectedReview === row.reviewId} onOpen={() => onReview(row.reviewId)}
            {...(selecting && eligible.includes(row) ? { selection: { checked: checked.includes(row.reviewId), onToggle: () => setChecked((existing) => existing.includes(row.reviewId) ? existing.filter((id) => id !== row.reviewId) : [...existing, row.reviewId]) } } : {})} /></li>
        : <li key={row.key}><button type="button" className="review-row" onClick={() => onTicket(row.key)}>
          <div className="row-top"><span className="ticket-key">{row.key}</span><span className="badge">No matching MR</span></div>
          <strong className="row-title">{row.title}</strong>
          <span className="row-foot"><span>{row.assignee ?? "Unassigned"} <span className="muted">Assignee</span></span><span className="muted">{row.status}{row.priority && ` · ${row.priority}`}</span></span>
        </button></li>)}
    </ul>}
    <Pagination trail={trail} next={page?.nextCursor} onPage={(next) => { setChecked([]); setTrail(next); }} />
  </>;
}
