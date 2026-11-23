import { useEffect, useEffectEvent, useRef, useState } from "react";
import type {
  CatalogItem,
  CatalogResponse,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
} from "../../api/generated/contracts/settings";
import { browseCatalog } from "../../api/settings";
import { Icon } from "../../components/Icon";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, readable } from "../../lib/presentation";
import { CatalogDialog } from "./catalog/CatalogDialog";
import { SavedFilterPicker } from "./catalog/SavedFilterPicker";
import { boardIncludesRepository, settingsFocus, toggleMembership } from "./scope-draft";
import type { CatalogConnection } from "./scope-draft";

type StatusCatalog = {
  readonly entries: ReadonlyMap<string, CatalogItem>;
  readonly next: string | null;
};
const boardReadBatch = 3;
type Props = {
  readonly active: boolean;
  readonly boards: readonly SaveSettingsBoardScope[];
  readonly repositories: readonly SaveSettingsRepositoryBinding[];
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly disabled: boolean;
  readonly browsing: boolean;
  readonly onDismissCatalog: () => void;
  readonly onRepositories: () => void;
  readonly showErrors: boolean;
  readonly onAdd: (items: readonly CatalogItem[]) => void;
  readonly onChange: (board: SaveSettingsBoardScope) => void;
  readonly onRemove: (scopeId: string) => void;
};

export function TeamBoards({
  active,
  boards,
  repositories,
  connection,
  connectionEpoch,
  disabled,
  browsing,
  onDismissCatalog,
  onRepositories,
  showErrors,
  onAdd,
  onChange,
  onRemove,
}: Props) {
  const [metadata, setMetadata] = useState<ReadonlyMap<string, CatalogItem>>(() => new Map());
  const [statuses, setStatuses] = useState<StatusCatalog>();
  const [statusError, setStatusError] = useState<string>();
  const [readingStatuses, setReadingStatuses] = useState(false);
  const enabledCount = boards.filter((board) => board.enabled).length;
  const statusRequest = useRef<AbortController | undefined>(undefined);

  useEffect(
    () => () => {
      statusRequest.current?.abort();
    },
    [],
  );

  async function add(items: readonly CatalogItem[], modalSignal: AbortSignal) {
    const requests = new AbortController();
    const signal = AbortSignal.any([modalSignal, requests.signal]);
    const verified: CatalogItem[] = [];
    try {
      for (let start = 0; start < items.length; start += boardReadBatch) {
        signal.throwIfAborted();
        const batch = await Promise.all(
          items.slice(start, start + boardReadBatch).map(async (item) => {
            try {
              return (await readBoard(connection, item.id, signal)).data;
            } catch (error) {
              signal.throwIfAborted();
              throw new Error(
                `Could not verify ${item.label} (#${item.id}). ${describeError(error)}`,
                { cause: error },
              );
            }
          }),
        );
        verified.push(...batch);
      }
      signal.throwIfAborted();
      setMetadata(
        (known) => new Map([...known, ...verified.map((item) => [item.id, item] as const)]),
      );
      onAdd(verified);
    } finally {
      requests.abort();
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

  const loadStatuses = useEffectEvent(() => void readStatuses());
  useEffect(() => {
    if (!active || !boards.length) return;
    const frame = requestAnimationFrame(() => loadStatuses());
    return () => cancelAnimationFrame(frame);
  }, [active, boards.length]);

  return (
    <section className="settings-section stack" aria-labelledby="team-boards">
      {browsing && active && (
        <CatalogDialog
          connectionEpoch={connectionEpoch}
          connection={connection}
          kind="boards"
          added={boards.map((board) => board.boardId)}
          tracked={boards.map((board) => ({
            id: board.boardId,
            label: board.name,
            detail: [metadata.get(board.boardId)?.path, readable(board.type)]
              .filter(Boolean)
              .join(" · "),
          }))}
          onClose={onDismissCatalog}
          onAdd={add}
        />
      )}
      <div className="settings-inline">
        <h3 id="team-boards">Tracked boards</h3>
        <span className="meta">
          {enabledCount} active {enabledCount === 1 ? "board" : "boards"}
        </span>
      </div>
      {boards.map((board, index) => (
        <BoardEditor
          key={board.scopeId}
          board={board}
          active={active}
          initiallyOpen={index === 0}
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
            settingsFocus("settings-add-board");
          }}
          onRepositories={onRepositories}
        />
      ))}
      {!boards.length && <p className="meta">Add a Jira board to configure review scope.</p>}
    </section>
  );
}

function BoardEditor({
  active,
  board,
  initiallyOpen,
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
  onRepositories,
}: {
  readonly active: boolean;
  readonly board: SaveSettingsBoardScope;
  readonly initiallyOpen: boolean;
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
  readonly onRepositories: Props["onRepositories"];
}) {
  const [opened, setOpened] = useState(initiallyOpen);
  const choose = active && opened;
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
  const repositoryCount = repositories.filter((repo) =>
    boardIncludesRepository(board, repo),
  ).length;
  const blankStatus = showErrors && board.enabled && !board.reviewStatusIds.length;
  const blankRepo = showErrors && board.enabled && !repositoryCount;

  return (
    <details
      className="settings-selection"
      open={initiallyOpen}
      onToggle={(event) => setOpened(event.currentTarget.open)}
    >
      <summary id={`settings-board-${board.boardId}`}>
        <span className="settings-resource-icon">
          <Icon name="board" />
        </span>
        <span className="settings-resource-name">
          <strong>{board.name}</strong>
          <small>
            {readable(exact?.boardType ?? board.type)} #{board.boardId}
            {!!board.jiraProjectKeys.length && ` · ${board.jiraProjectKeys.join(", ")}`}
            {" · "}
            {repositoryCount} {repositoryCount === 1 ? "repo" : "repos"}
          </small>
        </span>
        <span className="settings-state" data-active={board.enabled || undefined}>
          {board.enabled ? "Active" : "Paused"}
        </span>
        <Icon name="chevronRight" size={14} />
      </summary>
      <fieldset className="settings-selection-body stack" disabled={disabled}>
        <label className="settings-check">
          <input
            type="checkbox"
            checked={board.enabled}
            onChange={(event) => onChange({ ...board, enabled: event.currentTarget.checked })}
          />
          Include this board in the review queue
        </label>
        <h4>Ticket statuses in review</h4>
        {blankStatus && <Notice>Choose at least one review status.</Notice>}
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
        {blankRepo && (
          <Notice>
            <button type="button" className="quiet-button" onClick={onRepositories}>
              Manage Jira boards on Repositories
            </button>
          </Notice>
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
            <SavedFilterPicker
              connectionEpoch={connectionEpoch}
              connection={connection}
              current={board.teamFilterId}
              disabled={disabled}
              onAdd={(item) => {
                setFilterLabel(item.label);
                onChange({ ...board, teamFilterId: item.id });
              }}
            />
          </div>
        </details>
        <button className="quiet-button settings-remove" type="button" onClick={onRemove}>
          Remove board
        </button>
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
