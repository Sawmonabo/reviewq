import { useEffect, useRef, useState } from "react";
import type {
  CatalogItem,
  CatalogResponse,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
} from "../../api/generated/contracts/settings";
import { browseCatalog } from "../../api/settings";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, readable } from "../../lib/presentation";
import { CatalogPicker } from "./CatalogPicker";
import { boardIncludesRepository, settingsFocus, toggleMembership } from "./scope-draft";
import type { CatalogConnection } from "./scope-draft";

type StatusCatalog = {
  readonly entries: ReadonlyMap<string, CatalogItem>;
  readonly next: string | null;
};
type Props = {
  readonly boards: readonly SaveSettingsBoardScope[];
  readonly repositories: readonly SaveSettingsRepositoryBinding[];
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly disabled: boolean;
  readonly showErrors: boolean;
  readonly onResolving: (resolving: boolean) => void;
  readonly onAdd: (item: CatalogItem) => void;
  readonly onChange: (board: SaveSettingsBoardScope) => void;
  readonly onRemove: (scopeId: string) => void;
  readonly onMembership: (scopeId: string, projectId: string, include: boolean) => void;
};

export function TeamBoards({
  boards,
  repositories,
  connection,
  connectionEpoch,
  disabled,
  showErrors,
  onAdd,
  onChange,
  onRemove,
  onMembership,
  onResolving,
}: Props) {
  const [adding, setAdding] = useState<string>();
  const [addError, setAddError] = useState<string>();
  const [metadata, setMetadata] = useState<ReadonlyMap<string, CatalogItem>>(() => new Map());
  const [statuses, setStatuses] = useState<StatusCatalog>();
  const [statusError, setStatusError] = useState<string>();
  const [readingStatuses, setReadingStatuses] = useState(false);
  const addRequest = useRef<AbortController | undefined>(undefined);
  const statusRequest = useRef<AbortController | undefined>(undefined);

  useEffect(
    () => () => {
      addRequest.current?.abort();
      statusRequest.current?.abort();
      onResolving(false);
    },
    [onResolving],
  );

  async function add(item: CatalogItem) {
    if (disabled || addRequest.current) return;
    const controller = new AbortController();
    addRequest.current = controller;
    onResolving(true);
    setAdding(item.label);
    setAddError(undefined);
    try {
      const exact = await readBoard(connection, item.id, controller.signal);
      if (!controller.signal.aborted) {
        setMetadata((known) => new Map(known).set(item.id, exact.data));
        onAdd(exact.data);
      }
    } catch (error) {
      if (!controller.signal.aborted) setAddError(describeError(error));
    } finally {
      if (addRequest.current === controller) {
        addRequest.current = undefined;
        if (!controller.signal.aborted) {
          setAdding(undefined);
          onResolving(false);
        }
      }
    }
  }

  async function readStatuses(more = false) {
    if (disabled || statusRequest.current || (more ? !statuses?.next : statuses)) return;
    const controller = new AbortController();
    statusRequest.current = controller;
    setReadingStatuses(true);
    setStatusError(undefined);
    const cursor = more ? statuses?.next : undefined;
    try {
      const { data } = await browseCatalog(
        { ...connection, kind: "statuses", ...(cursor ? { cursor } : {}) },
        controller.signal,
      );
      if (controller.signal.aborted) return;
      if (cursor && data.nextCursor === cursor) throw new Error("Jira repeated a status page.");
      const entries = new Map(more ? statuses?.entries : undefined);
      for (const item of data.items) entries.set(item.id, item);
      setStatuses({ entries, next: data.nextCursor });
    } catch (error) {
      if (!controller.signal.aborted) setStatusError(describeError(error));
    } finally {
      if (statusRequest.current === controller) {
        statusRequest.current = undefined;
        if (!controller.signal.aborted) setReadingStatuses(false);
      }
    }
  }

  return (
    <section className="settings-section stack" aria-labelledby="team-boards">
      <h3 id="team-boards">Team boards</h3>
      <p className="meta">
        Only tickets from the boards and repositories you choose enter your queue.
      </p>
      <CatalogPicker
        connectionEpoch={connectionEpoch}
        scope={connection}
        kind="boards"
        label="Browse Jira boards"
        inputId="settings-board-search"
        unavailable={boards.map((board) => board.boardId)}
        disabled={disabled || Boolean(adding)}
        onAdd={(item) => void add(item)}
      />
      {adding && <Loading label={`Loading ${adding}…`} />}
      {addError && <Notice>{addError}</Notice>}
      {boards.map((board) => (
        <BoardEditor
          key={board.scopeId}
          board={board}
          repositories={repositories}
          connection={connection}
          connectionEpoch={connectionEpoch}
          metadata={metadata.get(board.boardId)}
          statuses={statuses}
          statusError={statusError}
          readingStatuses={readingStatuses}
          onReadStatuses={(more) => void readStatuses(more)}
          disabled={disabled}
          showErrors={showErrors}
          onChange={onChange}
          onRemove={() => {
            onRemove(board.scopeId);
            settingsFocus("settings-board-search");
          }}
          onMembership={onMembership}
        />
      ))}
      {!boards.length && (
        <p className="meta">Registered reviews remain available once connected.</p>
      )}
    </section>
  );
}

function BoardEditor({
  board,
  repositories,
  connection,
  connectionEpoch,
  metadata,
  statuses,
  statusError,
  readingStatuses,
  onReadStatuses,
  disabled,
  showErrors,
  onChange,
  onRemove,
  onMembership,
}: {
  readonly board: SaveSettingsBoardScope;
  readonly repositories: Props["repositories"];
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly metadata: CatalogItem | undefined;
  readonly statuses: StatusCatalog | undefined;
  readonly statusError: string | undefined;
  readonly readingStatuses: boolean;
  readonly onReadStatuses: (more?: boolean) => void;
  readonly disabled: boolean;
  readonly showErrors: boolean;
  readonly onChange: Props["onChange"];
  readonly onRemove: () => void;
  readonly onMembership: Props["onMembership"];
}) {
  const [choose, setChoose] = useState(false);
  const [projectKeys, setProjectKeys] = useState(board.jiraProjectKeys.join(", "));
  const [filterLabel, setFilterLabel] = useState<string>();
  const boardMetadata = useResource(`board-metadata:${board.boardId}:${choose}`, (signal) =>
    choose && !metadata
      ? readBoard(connection, board.boardId, signal)
      : Promise.resolve({ data: metadata, cursor: "" }),
  );
  const exact = metadata ?? boardMetadata.data;
  const allowed = new Set(exact?.statusIds);
  const available = [...(statuses?.entries.values() ?? [])].filter(
    (status) => !allowed.size || allowed.has(status.id),
  );
  const visible = new Set(available.map((item) => item.id));
  const unresolved = board.reviewStatusIds.filter((id) => !visible.has(id));
  const blankStatus = showErrors && board.enabled && !board.reviewStatusIds.length;
  const blankRepo =
    showErrors &&
    board.enabled &&
    !repositories.some((repo) => boardIncludesRepository(board, repo));

  return (
    <details className="settings-selection" open>
      <summary id={`settings-board-${board.boardId}`}>
        <strong>{board.name}</strong>
        <span>
          {readable(exact?.boardType ?? board.type)} · #{board.boardId}
          {!board.enabled && " · Paused"}
        </span>
      </summary>
      <fieldset className="settings-selection-body stack" disabled={disabled}>
        <div className="settings-inline">
          <label className="settings-check">
            <input
              type="checkbox"
              checked={board.enabled}
              onChange={(event) => onChange({ ...board, enabled: event.currentTarget.checked })}
            />
            Track this board
          </label>
          <button className="quiet-button" type="button" onClick={onRemove}>
            Remove
          </button>
        </div>
        <div className="settings-inline">
          <h4>Review statuses</h4>
          <button
            className="quiet-button"
            type="button"
            aria-expanded={choose}
            onClick={() => {
              setChoose(!choose);
              if (!choose) onReadStatuses();
            }}
          >
            {choose ? "Hide statuses" : "Choose statuses"}
          </button>
        </div>
        {blankStatus && <Notice>Choose at least one review status.</Notice>}
        {!choose && (
          <p className="meta">
            {board.reviewStatusIds.length
              ? `${board.reviewStatusIds.length} review ${board.reviewStatusIds.length === 1 ? "status" : "statuses"} selected`
              : "Choose the statuses that mean ready for review."}
          </p>
        )}
        {choose && (
          <div className="stack">
            {boardMetadata.error && (
              <Notice retry={boardMetadata.retry}>{boardMetadata.error}</Notice>
            )}
            {statusError && (
              <Notice retry={() => onReadStatuses(Boolean(statuses))}>{statusError}</Notice>
            )}
            {(boardMetadata.pending || readingStatuses) && (
              <Loading label="Reading Jira statuses…" />
            )}
            {exact && statuses && (
              <>
                <div className="settings-choices">
                  {available.map((status) => (
                    <label className="settings-check" key={status.id}>
                      <input
                        type="checkbox"
                        checked={board.reviewStatusIds.includes(status.id)}
                        onChange={(event) =>
                          onChange({
                            ...board,
                            reviewStatusIds: toggleMembership(
                              board.reviewStatusIds,
                              status.id,
                              event.currentTarget.checked,
                            ),
                          })
                        }
                      />
                      {status.label}
                    </label>
                  ))}
                  {unresolved.map((id) => (
                    <label className="settings-check" key={id}>
                      <input
                        type="checkbox"
                        checked
                        onChange={() =>
                          onChange({
                            ...board,
                            reviewStatusIds: toggleMembership(board.reviewStatusIds, id, false),
                          })
                        }
                      />
                      {statuses.entries.get(id)?.label ?? `Saved status #${id}`}
                      {allowed.size > 0 && !allowed.has(id) && (
                        <small> · No longer on this board</small>
                      )}
                    </label>
                  ))}
                </div>
                {!available.length && (
                  <p className="meta">
                    {statuses.next
                      ? "No board statuses in this page yet."
                      : "No Jira statuses are available for this board."}
                  </p>
                )}
                {statuses.next && (!allowed.size || allowed.size > visible.size) && (
                  <button
                    type="button"
                    className="quiet-button"
                    disabled={readingStatuses}
                    onClick={() => onReadStatuses(true)}
                  >
                    More Jira statuses
                  </button>
                )}
              </>
            )}
          </div>
        )}
        <h4>Repositories for this board</h4>
        {blankRepo && <Notice>Choose at least one repository.</Notice>}
        <div className="settings-choices">
          {repositories.map((repo) => (
            <label key={repo.projectId} className="settings-check">
              <input
                type="checkbox"
                checked={boardIncludesRepository(board, repo)}
                disabled={repo.boardSelection === "any"}
                onChange={(event) =>
                  onMembership(board.scopeId, repo.projectId, event.currentTarget.checked)
                }
              />
              <span>
                {repo.path}
                {repo.boardSelection === "any" && <small> · Any tracked board</small>}
              </span>
            </label>
          ))}
        </div>
        {!repositories.length && (
          <p className="meta">Add repositories below, then select the ones this team uses.</p>
        )}
        <details className="settings-advanced">
          <summary>Team restrictions &amp; estimates</summary>
          <div className="stack">
            <label className="field">
              Jira project keys
              <input
                value={projectKeys}
                placeholder="Optional · TEAM, PLATFORM"
                onChange={(event) => {
                  const value = event.currentTarget.value;
                  setProjectKeys(value);
                  onChange({
                    ...board,
                    jiraProjectKeys: [
                      ...new Set(
                        value
                          .toUpperCase()
                          .split(/[\s,]+/)
                          .filter(Boolean),
                      ),
                    ],
                  });
                }}
              />
            </label>
            <label className="field">
              Story points field
              <input
                value={board.storyPointsField ?? ""}
                placeholder={exact?.estimateField ?? "Use board estimation field"}
                onChange={(event) =>
                  onChange({ ...board, storyPointsField: event.currentTarget.value || null })
                }
              />
            </label>
            <div className="settings-inline">
              <h4>Saved team filter</h4>
              {board.teamFilterId && (
                <button
                  type="button"
                  className="quiet-button"
                  onClick={() => {
                    setFilterLabel(undefined);
                    onChange({ ...board, teamFilterId: null });
                  }}
                >
                  Clear {filterLabel ?? `filter #${board.teamFilterId}`}
                </button>
              )}
            </div>
            <p className="meta">Narrow a shared board to your team with an existing Jira filter.</p>
            <CatalogPicker
              connectionEpoch={connectionEpoch}
              scope={connection}
              kind="filters"
              label="Browse saved filters"
              unavailable={board.teamFilterId ? [board.teamFilterId] : []}
              onAdd={(item) => {
                setFilterLabel(item.label);
                onChange({ ...board, teamFilterId: item.id });
              }}
            />
          </div>
        </details>
      </fieldset>
    </details>
  );
}

async function readBoard(connection: CatalogConnection, id: string, signal: AbortSignal) {
  const result: CatalogResponse = await browseCatalog(
    { ...connection, kind: "board", boardId: id },
    signal,
  );
  const item = result.data.items.find((entry) => entry.id === id && entry.boardId === id);
  if (!item?.boardType) throw new Error("Jira did not verify this board or its type.");
  return { data: item, cursor: result.cursor };
}
