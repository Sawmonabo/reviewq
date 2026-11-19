import { useRef, useState } from "react";
import type { ReactNode } from "react";
import type { RunCard, RunStage } from "../../api/generated/contracts";
import { readArchive, readQueue, readQueueSync, readRuns } from "../../api/workspace";
import { Icon } from "../../components/Icon";
import { Pagination } from "../../components/Pagination";
import { Loading, Notice } from "../../components/PanelState";
import { providerLabel, ReviewBadges } from "../../components/ReviewRow";
import { localDateRange, presets } from "../../features/queue/archive-filters";
import type { DatePreset } from "../../features/queue/archive-filters";
import type { Launch, LaunchTarget, OpenReview } from "../../features/queue/types";
import { useResource } from "../../hooks/useResource";
import { formatActivityTime, reviewReasonLabel } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import { SelectionActions } from "../SelectionActions";
import { ArchiveDateFilter } from "./archive/ArchiveDateFilter";
import { ArchiveSprintFilter } from "./archive/ArchiveSprintFilter";
import "./runs.css";

const stages = [
  ["needs_input", "Awaiting input", "needsInput"],
  ["working", "Working", "working"],
  ["waiting", "Waiting", "waiting"],
  ["archived", "Archived", "archived"],
] as const;
const archiveDate = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const archiveDateSources = {
  decision: "Recorded review",
  report: "Accepted review report",
  followup: "Accepted review recheck",
};

export function RunsView({
  renderHeader,
  selectedReview,
  selecting,
  onSelect,
  onReview,
  onLaunch,
  onMonitor,
}: {
  readonly renderHeader: (selection?: ReactNode) => ReactNode;
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
  const [query, setQuery] = useState(() => {
    const saved = savedValue("runs.search");
    return (typeof saved === "string" ? saved : storedText("archive.search")).slice(0, 200);
  });
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
  function changeQuery(value: string) {
    setQuery(value);
    saveValue("runs.search", value);
    setTrail([]);
    setChecked([]);
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
      <div className="run-stage-tabs" role="group" aria-label="Filter review runs">
        {stages.map(([id, label, field]) => (
          <button
            type="button"
            key={id}
            aria-pressed={id === stage}
            aria-controls="review-runs-list"
            onClick={() => {
              if (id === stage) return;
              setStage(id);
              setTrail([]);
              setChecked([]);
              setQuestions(false);
              setReason("");
              saveValue("runs.stage", id);
              if (id === "archived" && selecting) onSelect(false);
            }}
          >
            <span>{label}</span>
            <strong>{(runs.data?.stageCounts ?? counts.data?.stageCounts)?.[field] ?? "—"}</strong>
          </button>
        ))}
      </div>
      {counts.error && !runs.data && <Notice retry={counts.retry}>{counts.error}</Notice>}
      {stage === "archived" ? (
        <ArchiveView
          selectedReview={selectedReview}
          onReview={onReview}
          query={query}
          onSearch={changeQuery}
        />
      ) : (
        <>
          <div className="run-search-controls">
            <label className="run-search">
              <Icon name="search" size={14} />
              <input
                type="search"
                aria-label="Search review runs"
                placeholder="Search ticket or title"
                value={query}
                maxLength={200}
                onChange={(event) => changeQuery(event.currentTarget.value)}
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
          <section
            id="review-runs-list"
            aria-label={`${stages.find(([id]) => id === stage)?.[1]} review runs`}
          >
            {runs.data && !runs.data.items.length && (
              <div className="run-empty" role="status">
                <h3>
                  {query.trim()
                    ? "No runs match your search"
                    : reason || questions
                      ? "No runs match these filters"
                      : {
                          needs_input: "Nothing awaiting your input",
                          working: "No runs working right now",
                          waiting: "No reviews waiting",
                        }[stage]}
                </h3>
                <p>
                  {query.trim()
                    ? "Try another ticket, title or MR number."
                    : stage === "waiting"
                      ? "Reviews waiting on author changes or merge appear here."
                      : "Start a review or choose another view."}
                </p>
              </div>
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
                      {...(selecting
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
          </section>
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
  query,
  onSearch,
}: {
  readonly selectedReview?: string | undefined;
  readonly onReview: OpenReview;
  readonly query: string;
  readonly onSearch: (value: string) => void;
}) {
  const search = useRef<HTMLInputElement>(null);
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
    q: query,
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
  const boardLabels = new Map(
    [...(catalog.data?.facets.boards ?? []), ...(archive.data?.boards ?? [])].map((board) => [
      board.id,
      board.label,
    ]),
  );

  function setField<T extends string>(key: string, setter: (newValue: T) => void, value: T) {
    setter(value);
    saveValue(key, value);
    setTrail([]);
  }
  const restricted = preset !== "all" || sprintPair.length > 0 || Boolean(query.trim());
  function clear() {
    onSearch("");
    setField("archive.date", changePreset, "all");
    setField("archive.start", changeStart, "");
    setField("archive.end", changeEnd, "");
    setPairs([]);
    saveValue("archive.pairs", []);
    search.current?.focus({ preventScroll: true });
  }

  return (
    <>
      <div className="archive-controls">
        <label className="run-search">
          <Icon name="search" size={14} />
          <input
            ref={search}
            type="search"
            aria-label="Search archive"
            placeholder="Search ticket or title"
            maxLength={200}
            value={query}
            onChange={(event) => {
              setTrail([]);
              onSearch(event.currentTarget.value);
            }}
          />
        </label>
        <ArchiveDateFilter
          value={{ preset, start, end }}
          onChange={(value) => {
            setField("archive.date", changePreset, value.preset);
            setField("archive.start", changeStart, value.start);
            setField("archive.end", changeEnd, value.end);
          }}
        />
        <ArchiveSprintFilter
          options={[...known].map(([id, label]) => {
            const board = id.split(":")[0] ?? "";
            return { id, label, board: boardLabels.get(board) ?? `Board ${board}` };
          })}
          selected={sprintPair}
          pending={archive.pending || catalog.pending}
          onChange={(next) => {
            setPairs(next);
            setTrail([]);
            saveValue("archive.pairs", next);
          }}
        />
        {restricted && (
          <button
            type="button"
            className="filter-control clear-filters"
            aria-label="Clear archive filters"
            title="Clear archive filters"
            onClick={clear}
          >
            <Icon name="clearFilters" size={17} />
          </button>
        )}
        {archive.pending && (
          <span className="archive-match-count" role="status">
            Loading…
          </span>
        )}
        {archive.data && (
          <span className="archive-match-count" role="status">
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
      <section id="review-runs-list" aria-label="Archived review runs">
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
            {!archive.data.items.length &&
              (restricted ? (
                <div className="archive-empty" role="status">
                  <Icon name="calendar" size={38} />
                  <h3>No reviews match these filters</h3>
                  <p>Try a different review date or sprint.</p>
                  <button type="button" onClick={clear}>
                    Clear filters
                  </button>
                </div>
              ) : (
                <div className="run-empty" role="status">
                  <h3>No archived reviews</h3>
                  <p>Merged and closed MRs appear here with their review history.</p>
                </div>
              ))}
            <ul className="run-items">
              {archive.data.items.map((row) => {
                const date = row.latestMatchingReviewAt;
                const sprintNames = row.sprintPairs
                  .filter(
                    ({ boardId, sprintId }) =>
                      !sprintPair.length || sprintPair.includes(`${boardId}:${sprintId}`),
                  )
                  .map(
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
                        <span className="archive-reviewed">
                          {date ? (
                            <time
                              dateTime={date}
                              title={`${row.dateSource ? archiveDateSources[row.dateSource] : "Review date"} · ${formatActivityTime(date)}`}
                            >
                              Reviewed {archiveDate.format(new Date(date))}
                            </time>
                          ) : (
                            "Review date unavailable"
                          )}
                          <span className="archive-sprint-name">
                            {sprintNames.length
                              ? sprintNames.join(", ")
                              : row.membership === "verified"
                                ? "No sprint"
                                : "Sprint unavailable"}
                            {row.membership === "partial" && " · Partial membership"}
                          </span>
                        </span>
                      }
                    />
                  </li>
                );
              })}
            </ul>
            <Pagination trail={trail} next={archive.data.nextCursor} onPage={setTrail} />
          </>
        )}
      </section>
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
  const kind = row.mrState === "open" ? reason : "archived";
  const label =
    row.mrState === "merged"
      ? "Merged"
      : row.mrState === "closed"
        ? "Closed"
        : reason
          ? reason === "awaiting_merge"
            ? "Awaiting merge"
            : reviewReasonLabel(reason)
          : undefined;
  const launch = row.focusedLaunch;
  const metadata = [
    row.focusedProvider && providerLabel(row.focusedProvider),
    launch?.observedModel ||
      (launch?.requestedModel && `Requested model: ${launch.requestedModel}`) ||
      (row.focusedProvider && "Model not reported"),
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
        aria-current={(!selection && selected) || undefined}
        role={selection ? "checkbox" : undefined}
        aria-checked={selection?.checked}
        aria-label={
          selection
            ? `Select ${row.tickets.map((ticket) => ticket.key).join(" · ") || row.projectPath} · !${row.iid}`
            : undefined
        }
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
            <span className="row-tag run-reason" data-review-kind={kind}>
              {label}
            </span>
          )}
        </div>
        <h3>{row.tickets[0]?.title || row.title}</h3>
        <p className="run-card-project">
          {row.focusedLeadName ?? `${row.projectPath} !${row.iid}`}
        </p>
        {row.registeredProviders.length > 1 && (
          <div className="actions" aria-label="Registered lead providers">
            <span className="provider-pill">
              {row.registeredProviders.map(providerLabel).join(" · ")}
            </span>
          </div>
        )}
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
          data-review-kind={kind}
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
