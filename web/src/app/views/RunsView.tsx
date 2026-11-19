import { useState } from "react";
import type { RunStage } from "../../api/generated/contracts";
import { readArchive, readQueue, readRuns } from "../../api/workspace";
import { Icon } from "../../components/Icon";
import { Pagination } from "../../components/Pagination";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { ReviewRow } from "../../components/ReviewRow";
import { localDateRange, presets } from "../../features/queue/archive-filters";
import type { DatePreset } from "../../features/queue/archive-filters";
import type { Launch, LaunchTarget, OpenReview } from "../../features/queue/types";
import { useResource } from "../../hooks/useResource";
import { formatTime, readable } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";

const stages = [
  ["needs_input", "Awaiting input", "needsInput", "rose"],
  ["working", "Working", "working", "green"],
  ["waiting", "Waiting", "waiting", "amber"],
  ["archived", "Archived", "archived", ""],
] as const;

export function RunsView({ selectedReview, selecting, onSelect, onReview, onLaunch, onMonitor }: {
  readonly selectedReview?: string | undefined;
  readonly selecting: boolean;
  readonly onSelect: (next: boolean) => void;
  readonly onReview: OpenReview;
  readonly onLaunch: Launch;
  readonly onMonitor: (targets: readonly LaunchTarget[], pause: boolean) => void;
}) {
  const [stage, setStage] = useState<RunStage>(() => stages.find(([id]) => id === savedValue("runs.stage"))?.[0] ?? "needs_input");
  const [query, setQuery] = useState("");
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [checked, setChecked] = useState<readonly string[]>([]);
  const [questions, setQuestions] = useState(false);
  const [reason, setReason] = useState("");
  const filters = { stage, q: query, cursor: trail.at(-1), hasAgentQuestions: questions || undefined, reasonKind: reason || undefined };
  const counts = useResource("runs-counts", (signal) => readRuns({ stage: "needs_input", limit: 1 }, signal), [{ kind: "run" }, { kind: "review" }, { kind: "queue" }]);
  const runs = useResource(`runs:${JSON.stringify(filters)}`, (signal) => stage === "archived"
    ? Promise.resolve({ data: null, cursor: "" })
    : readRuns(filters, signal), [{ kind: "run" }, { kind: "review" }, { kind: "queue" }]);
  const eligible = (runs.data?.items ?? []).filter((row) => row.mrState === "open");
  const captured = eligible.filter((row) => checked.includes(row.reviewId));

  return <>
    <div className="stage-grid" aria-label="Run stage">
      {stages.map(([id, label, field, color]) => <button type="button" key={id} aria-pressed={id === stage} className="stage-control" onClick={() => {
        setStage(id); setTrail([]); setChecked([]); saveValue("runs.stage", id);
      }}><span className={`stage-dot ${color}`} /><span>{label}</span><strong>{(runs.data?.stageCounts ?? counts.data?.stageCounts)?.[field] ?? "—"}</strong></button>)}
    </div>
    {counts.error && !runs.data && <Notice retry={counts.retry}>{counts.error}</Notice>}
    {stage === "archived" ? <ArchiveView selectedReview={selectedReview} onReview={onReview} /> : <>
      <div className="filter-stack">
        <label className="search-field"><Icon name="search" /><input aria-label="Search review runs" placeholder="Search review runs" value={query} maxLength={200} onChange={(event) => { setQuery(event.currentTarget.value); setTrail([]); setChecked([]); }} /></label>
        <div className="actions">
          <label><input type="checkbox" checked={questions} onChange={(event) => { setQuestions(event.currentTarget.checked); setTrail([]); setChecked([]); }} /> Agent questions</label>
          {reason && <button type="button" onClick={() => { setReason(""); setTrail([]); }}>Clear reason · {readable(reason)}</button>}
        </div>
      </div>
      <div className="list-heading"><strong>{runs.data?.totalMatching ?? "—"}</strong> matching reviews</div>
      {selecting && <div className="selection-bar actions"><strong>{captured.length} selected</strong>
        <button type="button" disabled={!eligible.length} onClick={() => setChecked(eligible.map((row) => row.reviewId))}>Select all shown</button>
        <button type="button" className="primary" disabled={!captured.length} onClick={() => onLaunch("prepare", captured)}>Prepare reviews</button>
        <button type="button" disabled={!captured.length} onClick={() => onMonitor(captured, false)}>Auto-review updates</button>
        <button type="button" disabled={!captured.length} onClick={() => onMonitor(captured, true)}>Pause auto-review</button>
        <button type="button" onClick={() => { setChecked([]); onSelect(false); }}>Cancel</button>
      </div>}
      {runs.error && <Notice retry={runs.retry}>{runs.error}{trail.length > 0 && <button type="button" onClick={() => setTrail([])}>Restart query</button>}</Notice>}
      {runs.pending && <Loading label="Loading review runs…" />}
      {runs.data && !runs.data.items.length && <Empty title="No runs in this stage"><p>Other stages remain available above.</p></Empty>}
      {runs.data && runs.data.items.length > 0 && <ul className="review-list">{runs.data.items.map((row) => <li key={row.reviewId}>
        <ReviewRow row={row} selected={row.reviewId === selectedReview} onOpen={() => onReview(row.reviewId, row.focusedRunId ?? undefined)}
          detail={<span>{row.runIds.length} {row.runIds.length === 1 ? "run" : "runs"}</span>}
          {...(selecting && eligible.includes(row) ? { selection: { checked: checked.includes(row.reviewId), onToggle: () => setChecked((old) => old.includes(row.reviewId) ? old.filter((id) => id !== row.reviewId) : [...old, row.reviewId]) } } : {})}/>
        {row.reasonKind && <button type="button" className="row-reason-filter" onClick={() => { setReason(row.reasonKind ?? ""); setTrail([]); setChecked([]); }}>Filter by {readable(row.reasonKind)}</button>}
      </li>)}</ul>}
      <Pagination trail={trail} next={runs.data?.nextCursor} onPage={(next) => { setChecked([]); setTrail(next); }} />
    </>}
  </>;
}

function ArchiveView({ selectedReview, onReview }: { readonly selectedReview?: string | undefined; readonly onReview: OpenReview }) {
  const [q, changeSearch] = useState(() => storedText("archive.search").slice(0, 200));
  const [preset, changePreset] = useState<DatePreset>(() => {
    const saved = savedValue("archive.date");
    return typeof saved === "string" && Object.hasOwn(presets, saved) ? saved as DatePreset : "all";
  });
  const [start, changeStart] = useState(() => storedText("archive.start"));
  const [end, changeEnd] = useState(() => storedText("archive.end"));
  const [sprintPair, setPairs] = useState<readonly string[]>(() => {
    const saved = savedValue("archive.pairs");
    return Array.isArray(saved) ? saved.filter((value): value is string => typeof value === "string" && /^\d+:\d+$/.test(value)).slice(0, 32) : [];
  });
  const [trail, setTrail] = useState<readonly string[]>([]);
  const dates = localDateRange(preset, start, end);
  const filters = { q, fromInclusive: dates.fromInclusive, untilExclusive: dates.untilExclusive, sprintPair, cursor: trail.at(-1) };
  const archive = useResource(`archive:${dates.invalid ? "invalid" : JSON.stringify(filters)}`, (signal) => dates.invalid
    ? Promise.resolve({ data: null, cursor: "" })
    : readArchive(filters, signal), [{ kind: "run" }, { kind: "review" }, { kind: "settings" }]);
  const catalog = useResource("archive-sprints", (signal) => readQueue({ sprint: "any", state: "archive", ownership: "all", limit: 1 }, signal), [{ kind: "queue" }, { kind: "settings" }]);
  const known = new Map(catalog.data?.facets.sprints.map((sprint) => [`${sprint.boardId}:${sprint.id}`, sprint.label]));
  for (const row of archive.data?.items ?? []) {
    for (const pair of row.sprintPairs) {
      const id = `${pair.boardId}:${pair.sprintId}`;
      if (!known.has(id)) known.set(id, `Sprint ${pair.sprintId}`);
    }
  }
  for (const id of sprintPair) if (!known.has(id)) known.set(id, `Sprint ${id.split(":")[1]}`);

  function setField<T extends string>(key: string, setter: (newValue: T) => void, value: T) {
    setter(value); saveValue(key, value); setTrail([]);
  }

  return <>
    <div className="filter-stack">
      <label className="search-field"><Icon name="search" /><input aria-label="Search archive" placeholder="Search archived reviews" maxLength={200} value={q} onChange={(event) => setField("archive.search", changeSearch, event.currentTarget.value)} /></label>
      <div className="filter-bar">
        <label>Reviewed<select value={preset} onChange={(event) => setField("archive.date", changePreset, event.currentTarget.value as DatePreset)}>
          {Object.entries(presets).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
        </select></label>
        {preset === "custom" && <>
          <label>From<input type="date" value={start} onChange={(event) => setField("archive.start", changeStart, event.currentTarget.value)} /></label>
          <label>Through<input type="date" value={end} onChange={(event) => setField("archive.end", changeEnd, event.currentTarget.value)} /></label>
        </>}
        <details className="filter-popover"><summary>Sprints {sprintPair.length > 0 && `(${sprintPair.length})`} <Icon name="chevron" size={12} /></summary><div>
          {known.size ? [...known].map(([id, label]) => {
            const board = id.split(":")[0];
            return <label key={id}><input type="checkbox" checked={sprintPair.includes(id)} onChange={(event) => {
              const next = event.currentTarget.checked ? [...sprintPair, id].slice(0, 32) : sprintPair.filter((pair) => pair !== id);
              setPairs(next); setTrail([]); saveValue("archive.pairs", next);
            }} /> {label} · {archive.data?.boards.find((entry) => entry.id === board)?.label ?? `Board ${board}`}</label>;
          }) : <p>No qualified sprints found.</p>}
        </div></details>
      </div>
    </div>
    {dates.invalid && <Notice>Choose an end date on or after the start date.</Notice>}
    {archive.error && <Notice retry={archive.retry}>{archive.error}{trail.length > 0 && <button type="button" onClick={() => setTrail([])}>Restart query</button>}</Notice>}
    {archive.pending && !dates.invalid && <Loading label="Loading archive…" />}
    {archive.data && <>
      <div className="list-heading"><strong>{archive.data.totalMatching}</strong> of {archive.data.totalArchived} archived</div>
      {archive.data.boards.some((board) => !board.available) && <Notice>Sprint membership unavailable for {archive.data.boards.filter((board) => !board.available).map((board) => board.label).join(", ")}.</Notice>}
      {!archive.data.items.length && <Empty title="No archived reviews match these filters" />}
      <ul className="review-list">{archive.data.items.map((row) => <li key={row.reviewId}>
        <ReviewRow row={row} selected={row.reviewId === selectedReview} onOpen={() => onReview(row.reviewId, row.focusedRunId ?? row.runIds[0])}
          detail={<><span>{row.latestMatchingReviewAt ? `Reviewed ${formatTime(row.latestMatchingReviewAt)}` : "Review date unavailable"}</span>
          {row.membership !== "verified" ? <span>{readable(row.membership)} sprint membership</span> : !row.sprintPairs.length && <span>No sprint</span>}</>} />
      </li>)}</ul>
      <Pagination trail={trail} next={archive.data.nextCursor} onPage={setTrail} />
    </>}
  </>;
}

function storedText(key: string) {
  const saved = savedValue(key);
  return typeof saved === "string" ? saved : "";
}
