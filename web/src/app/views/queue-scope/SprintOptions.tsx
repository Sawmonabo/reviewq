import { useEffect, useRef, useState } from "react";
import { browseCatalog } from "../../../api/settings";
import type { BoardScope, SettingsView, SprintFacet } from "../../../api/generated/contracts";
import type { QueueFilters } from "../../../features/queue/filters";
import { describeError } from "../../../lib/presentation";
import { sprintModes } from "./sprint-modes";

type SprintChoice = { readonly id: string; readonly label: string; readonly state: string | null };
type EarlierPage = { readonly choices: readonly SprintChoice[]; readonly next: string | null };

export function SprintOptions({
  mode,
  pairs,
  boards,
  known,
  settings,
  busy,
  onChange,
}: {
  readonly mode: QueueFilters["sprint"];
  readonly pairs: readonly string[];
  readonly boards: readonly BoardScope[];
  readonly known: readonly SprintFacet[];
  readonly settings: SettingsView | undefined;
  readonly busy: boolean;
  readonly onChange: (mode: QueueFilters["sprint"], pairs: readonly string[]) => void;
}) {
  const [search, setSearch] = useState("");
  const [earlier, setEarlier] = useState<Readonly<Record<string, EarlierPage>>>({});
  const [loadingBoard, setLoading] = useState<string>();
  const [failure, setFailure] = useState<string>();
  const request = useRef<AbortController | undefined>(undefined);
  useEffect(() => () => request.current?.abort(), []);
  const jira = settings?.connections.jira;
  const query = search.trim().toLocaleLowerCase();

  async function browse(boardId: string) {
    if (!settings || !jira?.site || !jira.email || !jira.tokenType || loadingBoard) return;
    const controller = new AbortController();
    request.current = controller;
    setLoading(boardId);
    setFailure(undefined);
    try {
      const page = earlier[boardId];
      const { data } = await browseCatalog(
        {
          kind: "sprints",
          provider: "jira",
          boardId,
          expectedRevision: settings.settingsRevision,
          connection: { site: jira.site, email: jira.email, tokenType: jira.tokenType },
          states: ["closed"],
          token: { action: "keep" },
          cursor: page?.next ?? null,
        },
        controller.signal,
      );
      if (controller.signal.aborted) return;
      const choices = new Map(
        (page?.choices ?? [])
          .filter(({ id }) => pairs.includes(`${boardId}:${id}`))
          .map((choice) => [choice.id, choice]),
      );
      for (const { id, label, state } of data.items.filter((item) => item.boardId === boardId)) {
        choices.set(id, { id, label, state });
      }
      setEarlier({
        ...earlier,
        [boardId]: { choices: [...choices.values()], next: data.nextCursor },
      });
    } catch (error) {
      if (!controller.signal.aborted) setFailure(describeError(error));
    } finally {
      if (!controller.signal.aborted) setLoading(undefined);
    }
  }

  return (
    <>
      <fieldset className="queue-scope-group" aria-label="Sprint mode">
        {(Object.keys(sprintModes) as (keyof typeof sprintModes)[]).map((id) => (
          <label className="queue-scope-option" key={id}>
            <input
              type="radio"
              name="queue-sprint-mode"
              checked={mode === id}
              disabled={busy}
              onChange={() => onChange(id, [])}
            />
            {sprintModes[id]}
          </label>
        ))}
      </fieldset>
      <input
        aria-label="Search sprints"
        placeholder="Find a loaded sprint…"
        maxLength={200}
        value={search}
        onChange={(event) => setSearch(event.currentTarget.value)}
      />
      {mode === "selected" && (
        <div className="queue-sprint-selected" aria-label="Selected sprints">
          {pairs.map((key) => {
            const [boardId, sprintId] = key.split(":");
            const label =
              known.find((entry) => entry.boardId === boardId && entry.id === sprintId)?.label ??
              earlier[boardId ?? ""]?.choices.find((entry) => entry.id === sprintId)?.label ??
              `Sprint ${sprintId ?? ""}`;
            const board = boards.find((entry) => entry.boardId === boardId)?.name;
            return (
              <button
                key={key}
                type="button"
                aria-label={`Remove ${label}${board ? ` from ${board}` : ""}`}
                className="quiet-button"
                disabled={busy}
                onClick={() => {
                  const next = pairs.filter((item) => item !== key);
                  onChange(next.length ? "selected" : "active", next);
                }}
              >
                {label} ×
              </button>
            );
          })}
        </div>
      )}
      {boards.map((board) => {
        const page = earlier[board.boardId];
        const items = new Map((page?.choices ?? []).map((choice) => [choice.id, choice]));
        for (const { id, label } of known.filter((choice) => choice.boardId === board.boardId)) {
          if (!items.has(id)) items.set(id, { id, label, state: null });
        }
        const matching = [...items.values()].filter(({ label }) =>
          label.toLocaleLowerCase().includes(query),
        );
        return (
          <fieldset key={board.boardId} className="queue-scope-group">
            <legend>{board.name}</legend>
            {matching.map((choice) => {
              const id = `${board.boardId}:${choice.id}`;
              const checked = mode === "selected" && pairs.includes(id);
              return (
                <label className="queue-scope-option" key={choice.id}>
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={busy || (!checked && pairs.length >= 32)}
                    onChange={() => {
                      const next = checked ? pairs.filter((item) => item !== id) : [...pairs, id];
                      onChange(next.length ? "selected" : "active", next);
                    }}
                  />
                  <span>
                    {choice.label}
                    {choice.state && <small>{choice.state}</small>}
                  </span>
                </label>
              );
            })}
            {!matching.length && query && (
              <p className="queue-scope-no-results">No loaded sprints match</p>
            )}
            {page?.next !== null && jira?.configured && (
              <button
                className="quiet-button"
                type="button"
                disabled={loadingBoard !== undefined}
                onClick={() => void browse(board.boardId)}
              >
                {loadingBoard === board.boardId
                  ? "Loading…"
                  : page
                    ? "More earlier sprints"
                    : "Browse earlier sprints"}
              </button>
            )}
          </fieldset>
        );
      })}
      {failure && (
        <p className="queue-scope-error" role="alert">
          {failure}
        </p>
      )}
    </>
  );
}
