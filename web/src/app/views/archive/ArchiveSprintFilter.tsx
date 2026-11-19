import { useState } from "react";
import { Icon } from "../../../components/Icon";
import { FilterPopover } from "../FilterPopover";

type Sprint = { readonly id: string; readonly label: string; readonly board: string };

export function ArchiveSprintFilter({
  options,
  selected,
  pending,
  onChange,
}: {
  readonly options: readonly Sprint[];
  readonly selected: readonly string[];
  readonly pending: boolean;
  readonly onChange: (next: readonly string[]) => void;
}) {
  const label =
    selected.length === 1
      ? (options.find((option) => option.id === selected[0])?.label ?? "1 selected")
      : selected.length
        ? `${selected.length} selected`
        : "Any sprint";
  return (
    <FilterPopover
      label="Sprint"
      value={label}
      triggerLabel="Filter by sprint"
      popupLabel="Choose sprints"
      popupClass="archive-sprint-popover"
      icon={<Icon name="refresh" size={15} />}
    >
      {(close) => (
        <SprintOptions
          options={options}
          selected={selected}
          pending={pending}
          onChange={onChange}
          close={close}
        />
      )}
    </FilterPopover>
  );
}

function SprintOptions({
  options,
  selected,
  pending,
  onChange,
  close,
}: {
  readonly options: readonly Sprint[];
  readonly selected: readonly string[];
  readonly pending: boolean;
  readonly onChange: (next: readonly string[]) => void;
  readonly close: () => void;
}) {
  const [search, setSearch] = useState("");
  const query = search.trim().toLocaleLowerCase();
  const matching = options.filter(({ label, board }) =>
    `${label} ${board}`.toLocaleLowerCase().includes(query),
  );
  const boards = new Map<string, Sprint[]>();
  for (const sprint of matching) {
    const entries = boards.get(sprint.board) ?? [];
    entries.push(sprint);
    boards.set(sprint.board, entries);
  }
  return (
    <>
      <label className="archive-sprint-search">
        <Icon name="search" size={14} />
        <input
          aria-label="Find a sprint"
          placeholder="Find a sprint…"
          maxLength={200}
          value={search}
          autoFocus
          onChange={(event) => setSearch(event.currentTarget.value)}
        />
      </label>
      <button
        type="button"
        className="archive-sprint-any"
        aria-pressed={!selected.length}
        onClick={() => onChange([])}
      >
        Any sprint
        {!selected.length && <Icon name="check" size={13} />}
      </button>
      {[...boards].map(([board, sprints]) => (
        <fieldset key={board} className="archive-sprint-board">
          <legend>{board}</legend>
          {sprints.map((sprint) => {
            const checked = selected.includes(sprint.id);
            return (
              <label key={sprint.id} className="archive-sprint-option">
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={!checked && selected.length >= 32}
                  onChange={(event) =>
                    onChange(
                      event.currentTarget.checked
                        ? [...selected, sprint.id]
                        : selected.filter((id) => id !== sprint.id),
                    )
                  }
                />
                {sprint.label}
              </label>
            );
          })}
        </fieldset>
      ))}
      {!matching.length && (
        <p className="archive-sprint-empty">
          {pending
            ? "Loading sprints…"
            : search
              ? "No sprints match your search"
              : "No qualified sprints found"}
        </p>
      )}
      <div className="archive-popup-footer">
        <span>{selected.length ? `${selected.length} selected` : "Select one or more"}</span>
        <button className="quiet-button" type="button" onClick={close}>
          Done
        </button>
      </div>
    </>
  );
}
