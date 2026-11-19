import type { QueueFacets, SettingsView } from "../../../api/generated/contracts";
import type { QueueFilters } from "../../../features/queue/filters";
import type { resolveQueueScope } from "../../../features/queue/scope";
import { FilterPopover } from "../FilterPopover";
import { SprintOptions } from "./SprintOptions";
import { sprintModes } from "./sprint-modes";
import "./scope.css";

type Scope = ReturnType<typeof resolveQueueScope>;
type Choice = { readonly id: string; readonly label: string };

export function ScopeFilters({
  scope,
  settings,
  facets,
  loadingSprint,
  onChange,
  onSprint,
}: {
  readonly scope: Scope | undefined;
  readonly settings: SettingsView | undefined;
  readonly facets: QueueFacets | undefined;
  readonly loadingSprint: boolean;
  readonly onChange: (field: "boardIds" | "projectIds", ids: readonly string[] | null) => void;
  readonly onSprint: (mode: QueueFilters["sprint"], pairs: readonly string[]) => void;
}) {
  const {
    boardIds = null,
    projectIds = null,
    sprint = "active",
    sprintPairs = [],
  } = scope?.filters ?? {};
  const disabled = settings?.mode !== "connected";
  const boards = scope?.boards.map((board) => ({ id: board.boardId, label: board.name })) ?? [];
  const repositories =
    scope?.repositories
      .map((repo) => ({ id: repo.projectId, label: repo.path }))
      .sort((left, right) => left.label.localeCompare(right.label)) ?? [];
  const sprintLabel =
    sprint === "selected" ? `${sprintPairs.length} selected` : sprintModes[sprint];
  return (
    <>
      <ScopeChoices
        label="Board"
        all="All team boards"
        options={boards}
        selected={boardIds}
        disabled={disabled || !boards.length}
        onChange={(ids) => onChange("boardIds", ids)}
      />
      <FilterPopover
        label="Sprint"
        value={sprintLabel}
        triggerLabel={`Sprint: ${sprintLabel}`}
        popupLabel="Choose queue sprints"
        popupClass="queue-scope-popup queue-sprints"
        disabled={disabled || !scope?.scrum.length}
      >
        {() => (
          <SprintOptions
            key={settings?.settingsRevision}
            mode={sprint}
            pairs={sprintPairs}
            boards={scope?.scrum ?? []}
            known={facets?.sprints ?? []}
            settings={settings}
            busy={loadingSprint}
            onChange={onSprint}
          />
        )}
      </FilterPopover>
      <ScopeChoices
        label="Repo"
        all="All repositories"
        options={repositories}
        selected={projectIds}
        disabled={disabled || !repositories.length}
        onChange={(ids) => onChange("projectIds", ids)}
      />
    </>
  );
}

function ScopeChoices({
  label,
  all,
  options,
  selected,
  disabled,
  onChange,
}: {
  readonly label: "Board" | "Repo";
  readonly all: string;
  readonly options: readonly Choice[];
  readonly selected: readonly string[] | null;
  readonly disabled: boolean;
  readonly onChange: (ids: readonly string[] | null) => void;
}) {
  const chosen = selected ?? options.map(({ id }) => id);
  const value =
    selected === null
      ? all
      : label === "Board" && selected.length === 1
        ? (options.find(({ id }) => id === selected[0])?.label ?? "1 selected")
        : `${selected.length} selected`;
  return (
    <FilterPopover
      label={label}
      value={value}
      triggerLabel={`${label}: ${value}`}
      popupLabel={label === "Board" ? "Choose team boards" : "Choose repositories"}
      popupClass="queue-scope-popup"
      disabled={disabled}
    >
      {() => (
        <>
          <div className="queue-scope-actions">
            <button type="button" className="quiet-button" onClick={() => onChange(null)}>
              Select all
            </button>
            <button type="button" className="quiet-button" onClick={() => onChange([])}>
              Clear
            </button>
          </div>
          {options.map(({ id, label }) => {
            const checked = chosen.includes(id);
            return (
              <label key={id} className="queue-scope-option">
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={() => {
                    const next = checked ? chosen.filter((item) => item !== id) : [...chosen, id];
                    onChange(next.length === options.length ? null : next);
                  }}
                />
                {label}
              </label>
            );
          })}
        </>
      )}
    </FilterPopover>
  );
}
