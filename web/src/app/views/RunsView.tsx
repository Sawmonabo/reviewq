import { useState } from "react";
import type { ReactNode } from "react";
import type { RunCard, RunStage } from "../../api/generated/contracts";
import { readArchive, readQueue, readQueueSync, readRuns } from "../../api/workspace";
import { FilterSelect } from "../../components/FilterSelect";
import { Icon } from "../../components/Icon";
import { Pagination } from "../../components/Pagination";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { ReviewBadges } from "../../components/ReviewRow";
import { localDateRange, presets } from "../../features/queue/archive-filters";
import type { DatePreset } from "../../features/queue/archive-filters";
import type { Launch, LaunchTarget, OpenReview } from "../../features/queue/types";
import { useResource } from "../../hooks/useResource";
import {
  formatActivityTime,
  providerLabel,
  readable,
  reviewReasonLabel,
} from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import "./runs.css";

const stages = [
  ["needs_input", "Awaiting input", "needsInput"],
  ["working", "Working", "working"],
  ["waiting", "Waiting", "waiting"],
  ["archived", "Archived", "archived"],
] as const;

export function RunsView({
  selectedReview,
  selecting,
  onSelect,
  onReview,
  onLaunch,
  onMonitor,
}: {
  readonly selectedReview?: string | undefined;
  readonly selecting: boolean;
  readonly onSelect: (next: boolean) => void;
  readonly onReview: OpenReview;
  readonly onLaunch: Launch;
  readonly onMonitor: (targets: readonly LaunchTarget[], pause: boolean) => void;
}) {
  const [stage, setStage] = useState<RunStage>(
    () => stages.find(([id]) => id === savedValue("runs.stage"))?.[0] ?? "needs_input",
  );
  const [query, setQuery] = useState("");
  const [trail, setTrail] = useState<readonly string[]>([]);
  const [checked, setChecked] = useState<readonly string[]>([]);
  const [questions, setQuestions] = useState(false);
  const [reason, setReason] = useState("");
  const filters = {
    stage,
    q: query,
    cursor: trail.at(-1),
    hasAgentQuestions: questions || undefined,
    reasonKind: reason || undefined,
  };
  const counts = useResource(
    "runs-counts",
    (signal) => readRuns({ stage: "needs_input", limit: 1 }, signal),
    [{ kind: "run" }, { kind: "review" }, { kind: "queue" }],
  );
  const runs = useResource(
    `runs:${JSON.stringify(filters)}`,
    (signal) =>
      stage === "archived"
        ? Promise.resolve({ data: null, cursor: "" })
        : readRuns(filters, signal),
    [{ kind: "run" }, { kind: "review" }, { kind: "queue" }],
  );
  const sync = useResource("runs-queue-sync", readQueueSync, [{ kind: "queue_sync" }]).data;
  const eligible = (runs.data?.items ?? []).filter((row) => row.mrState === "open");
  const captured = eligible.filter((row) => checked.includes(row.reviewId));

  return (
    <>
      <div className="run-stage-tabs" aria-label="Filter review runs">
        {stages.map(([id, label, field]) => (
          <button
            type="button"
            key={id}
            aria-pressed={id === stage}
            onClick={() => {
              setStage(id);
              setTrail([]);
              setChecked([]);
              saveValue("runs.stage", id);
            }}
          >
            <span>{label}</span>
            <strong>{(runs.data?.stageCounts ?? counts.data?.stageCounts)?.[field] ?? "—"}</strong>
          </button>
        ))}
      </div>
      {counts.error && !runs.data && <Notice retry={counts.retry}>{counts.error}</Notice>}
      {stage === "archived" ? (
        <ArchiveView selectedReview={selectedReview} onReview={onReview} />
      ) : (
        <>
          <div className="run-search-controls">
            <label className="run-search">
              <Icon name="search" size={14} />
              <input
                aria-label="Search review runs"
                placeholder="Search ticket or title"
                value={query}
                maxLength={200}
                onChange={(event) => {
                  setQuery(event.currentTarget.value);
                  setTrail([]);
                  setChecked([]);
                }}
              />
            </label>
            {(stage === "working" || questions) && (
              <label className="run-question-filter">
                <input
                  type="checkbox"
                  checked={questions}
                  onChange={(event) => {
                    setQuestions(event.currentTarget.checked);
                    setTrail([]);
                    setChecked([]);
                  }}
                />
                Agent questions
              </label>
            )}
            {reason && (
              <button
                type="button"
                className="filter-control"
                onClick={() => {
                  setReason("");
                  setTrail([]);
                }}
              >
                Clear reason · {reviewReasonLabel(reason)}
              </button>
            )}
          </div>
          {selecting && (
            <div className="selection-bar actions">
              <strong>{captured.length} selected</strong>
              <button
                type="button"
                disabled={!eligible.length}
                onClick={() => setChecked(eligible.map((row) => row.reviewId))}
              >
                Select all shown
              </button>
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
          {runs.error && (
            <Notice retry={runs.retry}>
              {runs.error}
              {trail.length > 0 && (
                <button type="button" onClick={() => setTrail([])}>
                  Restart query
                </button>
              )}
            </Notice>
          )}
          {runs.pending && <Loading label="Loading review runs…" />}
          {runs.data && !runs.data.items.length && (
            <Empty title="No runs in this stage">
              <p>Other stages remain available above.</p>
            </Empty>
          )}
          {runs.data && runs.data.items.length > 0 && (
            <ul className="run-items">
              {runs.data.items.map((row) => (
                <li key={row.reviewId}>
                  <RunRow
                    row={row}
                    selected={row.reviewId === selectedReview}
                    disabled={selecting && !eligible.includes(row)}
                    onOpen={() => onReview(row.reviewId, row.focusedRunId ?? undefined)}
                    onReason={
                      row.reasonKind
                        ? () => {
                            setReason(row.reasonKind ?? "");
                            setTrail([]);
                            setChecked([]);
                          }
                        : undefined
                    }
                    {...(selecting && eligible.includes(row)
                      ? {
                          selection: {
                            checked: checked.includes(row.reviewId),
                            onToggle: () =>
                              setChecked((old) =>
                                old.includes(row.reviewId)
                                  ? old.filter((id) => id !== row.reviewId)
                                  : [...old, row.reviewId],
                              ),
                          },
                        }
                      : {})}
                  />
                </li>
              ))}
            </ul>
          )}
          <Pagination
            trail={trail}
            next={runs.data?.nextCursor}
            onPage={(next) => {
              setChecked([]);
              setTrail(next);
            }}
          />
        </>
      )}
      {sync?.lastSuccessAt && (
        <footer className="workspace-sync">
          <time dateTime={sync.lastSuccessAt}>
            {sync.stale ? "Last successful sync" : "Last updated"}:{" "}
            {formatActivityTime(sync.lastSuccessAt)}
          </time>
        </footer>
      )}
    </>
  );
}

function ArchiveView({
  selectedReview,
  onReview,
}: {
  readonly selectedReview?: string | undefined;
  readonly onReview: OpenReview;
}) {
  const [q, changeSearch] = useState(() => storedText("archive.search").slice(0, 200));
  const [preset, changePreset] = useState<DatePreset>(() => {
    const saved = savedValue("archive.date");
    return typeof saved === "string" && Object.hasOwn(presets, saved)
      ? (saved as DatePreset)
      : "all";
  });
  const [start, changeStart] = useState(() => storedText("archive.start"));
  const [end, changeEnd] = useState(() => storedText("archive.end"));
  const [sprintPair, setPairs] = useState<readonly string[]>(() => {
    const saved = savedValue("archive.pairs");
    return Array.isArray(saved)
      ? saved
          .filter((value): value is string => typeof value === "string" && /^\d+:\d+$/.test(value))
          .slice(0, 32)
      : [];
  });
  const [trail, setTrail] = useState<readonly string[]>([]);
  const dates = localDateRange(preset, start, end);
  const filters = {
    q,
    fromInclusive: dates.fromInclusive,
    untilExclusive: dates.untilExclusive,
    sprintPair,
    cursor: trail.at(-1),
  };
  const archive = useResource(
    `archive:${dates.invalid ? "invalid" : JSON.stringify(filters)}`,
    (signal) =>
      dates.invalid ? Promise.resolve({ data: null, cursor: "" }) : readArchive(filters, signal),
    [{ kind: "run" }, { kind: "review" }, { kind: "settings" }],
  );
  const catalog = useResource(
    "archive-sprints",
    (signal) => readQueue({ sprint: "any", state: "archive", ownership: "all", limit: 1 }, signal),
    [{ kind: "queue" }, { kind: "settings" }],
  );
  const known = new Map(
    catalog.data?.facets.sprints.map((sprint) => [`${sprint.boardId}:${sprint.id}`, sprint.label]),
  );
  for (const row of archive.data?.items ?? []) {
    for (const pair of row.sprintPairs) {
      const id = `${pair.boardId}:${pair.sprintId}`;
      if (!known.has(id)) known.set(id, `Sprint ${pair.sprintId}`);
    }
  }
  for (const id of sprintPair) if (!known.has(id)) known.set(id, `Sprint ${id.split(":")[1]}`);

  function setField<T extends string>(key: string, setter: (newValue: T) => void, value: T) {
    setter(value);
    saveValue(key, value);
    setTrail([]);
  }

  return (
    <>
      <div className="archive-controls">
        <label className="run-search">
          <Icon name="search" size={14} />
          <input
            aria-label="Search archive"
            placeholder="Search ticket or title"
            maxLength={200}
            value={q}
            onChange={(event) =>
              setField("archive.search", changeSearch, event.currentTarget.value)
            }
          />
        </label>
        <FilterSelect
          label="Reviewed"
          value={preset}
          options={Object.entries(presets).map(([value, label]) => ({ value, label }))}
          onChange={(value) => setField("archive.date", changePreset, value as DatePreset)}
        />
        {preset === "custom" && (
          <>
            <label>
              From
              <input
                type="date"
                value={start}
                onChange={(event) =>
                  setField("archive.start", changeStart, event.currentTarget.value)
                }
              />
            </label>
            <label>
              Through
              <input
                type="date"
                value={end}
                onChange={(event) => setField("archive.end", changeEnd, event.currentTarget.value)}
              />
            </label>
          </>
        )}
        <details className="filter-popover queue-hide">
          <summary className="filter-control">
            <span className="filter-label">Sprint</span>
            {sprintPair.length ? `${sprintPair.length} selected` : "All sprints"}
            <Icon name="chevron" size={12} />
          </summary>
          <div>
            {known.size ? (
              [...known].map(([id, label]) => {
                const board = id.split(":")[0];
                return (
                  <label key={id}>
                    <input
                      type="checkbox"
                      checked={sprintPair.includes(id)}
                      onChange={(event) => {
                        const next = event.currentTarget.checked
                          ? [...sprintPair, id].slice(0, 32)
                          : sprintPair.filter((pair) => pair !== id);
                        setPairs(next);
                        setTrail([]);
                        saveValue("archive.pairs", next);
                      }}
                    />{" "}
                    {label} ·{" "}
                    {archive.data?.boards.find((entry) => entry.id === board)?.label ??
                      `Board ${board}`}
                  </label>
                );
              })
            ) : (
              <p>No qualified sprints found.</p>
            )}
          </div>
        </details>
        {(preset !== "all" || sprintPair.length > 0 || q) && (
          <button
            type="button"
            className="filter-control clear-filters"
            aria-label="Clear archive filters"
            title="Clear archive filters"
            onClick={() => {
              changeSearch("");
              changePreset("all");
              changeStart("");
              changeEnd("");
              setPairs([]);
              setTrail([]);
              saveValue("archive.search", "");
              saveValue("archive.date", "all");
              saveValue("archive.start", "");
              saveValue("archive.end", "");
              saveValue("archive.pairs", []);
            }}
          >
            <Icon name="clearFilters" size={17} />
          </button>
        )}
        {archive.data && (
          <span className="archive-match-count">
            {archive.data.totalMatching} {archive.data.totalMatching === 1 ? "review" : "reviews"}
          </span>
        )}
      </div>
      {dates.invalid && <Notice>Choose an end date on or after the start date.</Notice>}
      {archive.error && (
        <Notice retry={archive.retry}>
          {archive.error}
          {trail.length > 0 && (
            <button type="button" onClick={() => setTrail([])}>
              Restart query
            </button>
          )}
        </Notice>
      )}
      {archive.pending && !dates.invalid && <Loading label="Loading archive…" />}
      {archive.data && (
        <>
          {archive.data.boards.some((board) => !board.available) && (
            <Notice>
              Sprint membership unavailable for{" "}
              {archive.data.boards
                .filter((board) => !board.available)
                .map((board) => board.label)
                .join(", ")}
              .
            </Notice>
          )}
          {!archive.data.items.length && <Empty title="No archived reviews match these filters" />}
          <ul className="run-items">
            {archive.data.items.map((row) => {
              const date = row.latestMatchingReviewAt;
              const sprintNames = row.sprintPairs.map(
                ({ boardId, sprintId }) =>
                  known.get(`${boardId}:${sprintId}`) ?? `Sprint ${sprintId}`,
              );
              return (
                <li key={row.reviewId}>
                  <RunRow
                    row={row}
                    selected={row.reviewId === selectedReview}
                    onOpen={() => onReview(row.reviewId, row.focusedRunId ?? row.runIds[0])}
                    archiveDetails={
                      <>
                        {date ? (
                          <time dateTime={date} title={formatActivityTime(date)}>
                            Reviewed {new Date(date).toLocaleDateString()}
                          </time>
                        ) : (
                          "Review date unavailable"
                        )}
                        {" · "}
                        {row.membership !== "verified"
                          ? `${readable(row.membership)} sprint membership`
                          : sprintNames.length
                            ? sprintNames.join(", ")
                            : "No sprint"}
                      </>
                    }
                  />
                </li>
              );
            })}
          </ul>
          <Pagination trail={trail} next={archive.data.nextCursor} onPage={setTrail} />
        </>
      )}
    </>
  );
}

function RunRow({
  row,
  selected,
  disabled = false,
  onOpen,
  onReason,
  selection,
  archiveDetails,
}: {
  readonly row: RunCard;
  readonly selected: boolean;
  readonly disabled?: boolean;
  readonly onOpen: () => void;
  readonly onReason?: (() => void) | undefined;
  readonly selection?: { readonly checked: boolean; readonly onToggle: () => void };
  readonly archiveDetails?: ReactNode;
}) {
  const reason = row.reasonKind ?? row.runStage;
  const label = reason ? reviewReasonLabel(reason) : undefined;
  const launch = row.focusedLaunch;
  const metadata = [
    row.focusedProvider && providerLabel(row.focusedProvider),
    launch?.observedModel ||
      (launch?.requestedModel && `Requested model: ${launch.requestedModel}`),
    launch?.observedEffort ||
      (launch?.requestedEffort && `Requested effort: ${launch.requestedEffort}`),
    row.focusedReportReviewedSha && `Report reviewed ${row.focusedReportReviewedSha.slice(0, 8)}`,
  ].filter(Boolean);
  return (
    <article className="run-card">
      <button
        type="button"
        className="run-card-open"
        disabled={disabled}
        aria-current={selected || undefined}
        aria-pressed={selection?.checked}
        onClick={selection?.onToggle ?? onOpen}
      >
        <div className="run-card-head">
          {selection && (
            <span className="row-checkbox" aria-hidden="true">
              {selection.checked && <Icon name="check" size={13} />}
            </span>
          )}
          {row.tickets.length > 0 && (
            <span className="ticket-key">
              {row.tickets.map((ticket) => ticket.key).join(" · ")}
            </span>
          )}
          <span>!{row.iid}</span>
          <span>
            {row.runIds.length} {row.runIds.length === 1 ? "run" : "runs"}
          </span>
          <ReviewBadges review={row} />
          {row.runStage === "working" && row.hasAgentQuestions && (
            <span className="row-tag" data-review-kind="agent_input">
              Question pending
            </span>
          )}
          {label && (!onReason || Boolean(selection)) && (
            <span className="row-tag run-reason" data-review-kind={reason}>
              {label}
            </span>
          )}
        </div>
        <h3>{row.tickets[0]?.title || row.title}</h3>
        <p className="run-card-project">
          {row.tickets[0]?.title && row.tickets[0].title !== row.title
            ? row.title
            : `${row.projectPath} !${row.iid}`}
        </p>
        <div className="run-card-footer">
          <span>{metadata.join(" · ")}</span>
          <span>
            {archiveDetails ??
              (row.lastActivityAt && (
                <time dateTime={row.lastActivityAt}>{formatActivityTime(row.lastActivityAt)}</time>
              ))}
          </span>
        </div>
      </button>
      {label && onReason && !selection && (
        <button
          type="button"
          className="row-tag run-reason run-reason-filter"
          data-review-kind={reason}
          aria-label={`Filter by ${label}`}
          onClick={onReason}
        >
          {label}
        </button>
      )}
    </article>
  );
}

function storedText(key: string) {
  const saved = savedValue(key);
  return typeof saved === "string" ? saved : "";
}
