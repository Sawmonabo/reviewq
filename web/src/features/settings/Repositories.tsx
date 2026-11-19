import type {
  CatalogItem,
  GitlabDraft,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
} from "../../api/generated/contracts/settings";
import { CatalogPicker } from "./CatalogPicker";
import { CheckoutSkill } from "./CheckoutSkill";
import { boardIncludesRepository, settingsFocus } from "./scope-draft";
import type { CatalogConnection } from "./scope-draft";

export function Repositories({
  boards,
  repositories,
  gitlab,
  connection,
  connectionEpoch,
  disabled,
  onAdd,
  onChange,
  onRemove,
  onMembership,
}: {
  readonly boards: readonly SaveSettingsBoardScope[];
  readonly repositories: readonly SaveSettingsRepositoryBinding[];
  readonly gitlab: GitlabDraft;
  readonly connection: CatalogConnection;
  readonly connectionEpoch: number;
  readonly disabled: boolean;
  readonly onAdd: (item: CatalogItem) => void;
  readonly onChange: (repo: SaveSettingsRepositoryBinding) => void;
  readonly onRemove: (id: string) => void;
  readonly onMembership: (scopeId: string, projectId: string, include: boolean) => void;
}) {
  return (
    <section className="settings-section stack" aria-labelledby="settings-repositories">
      <h3 id="settings-repositories">Repositories &amp; agents</h3>
      <CatalogPicker
        kind="projects"
        label="Browse repositories"
        inputId="settings-repository-search"
        scope={connection}
        connectionEpoch={connectionEpoch}
        disabled={disabled}
        unavailable={repositories.map((repo) => repo.projectId)}
        onAdd={onAdd}
      />
      {repositories.map((repo) => (
        <details className="settings-selection" key={repo.projectId} open>
          <summary id={`settings-repository-${repo.projectId}`}>
            <strong>{repo.path}</strong>
            <span>#{repo.projectId}</span>
          </summary>
          <fieldset className="settings-selection-body stack" disabled={disabled}>
            <div className="settings-inline">
              <p className="meta">Choose a local checkout for agents and ReviewQ chats.</p>
              <button
                type="button"
                className="quiet-button"
                onClick={() => {
                  onRemove(repo.projectId);
                  settingsFocus("settings-repository-search");
                }}
              >
                Remove
              </button>
            </div>
            <label className="field">
              Ticket boards
              <select
                aria-label={`Ticket boards for ${repo.path}`}
                value={repo.boardSelection}
                onChange={(event) =>
                  onChange({
                    ...repo,
                    boardSelection: event.currentTarget.value === "any" ? "any" : "selected",
                  })
                }
              >
                <option value="selected">Selected boards</option>
                <option value="any">Any tracked board</option>
              </select>
            </label>
            {repo.boardSelection === "any" && (
              <p className="meta">
                Use tickets from any board you follow, including boards you add later.
              </p>
            )}
            {boards.length > 0 && (
              <div className="settings-choices">
                {boards.map((board) => (
                  <label key={board.scopeId} className="settings-check">
                    <input
                      type="checkbox"
                      checked={boardIncludesRepository(board, repo)}
                      disabled={repo.boardSelection === "any"}
                      onChange={(event) =>
                        onMembership(board.scopeId, repo.projectId, event.currentTarget.checked)
                      }
                    />
                    <span>
                      {board.name}
                      {!board.enabled && <small> · Paused</small>}
                    </span>
                  </label>
                ))}
              </div>
            )}
            {!boards.some((board) => board.enabled && boardIncludesRepository(board, repo)) && (
              <p className="meta">
                {boards.length
                  ? "Select a team board to include this repository in your review queue."
                  : "Add a team board above to include this repository in your review queue."}
              </p>
            )}
            <label className="field">
              Review checkout
              <input
                id={`checkout-${repo.projectId}`}
                value={repo.checkout ?? ""}
                placeholder="/home/you/repos/project"
                onChange={(event) =>
                  onChange({
                    ...repo,
                    checkout: event.currentTarget.value || null,
                    defaultSkills: {},
                  })
                }
              />
            </label>
            {repo.checkout && (
              <div className="fields settings-default-skills">
                {(["claude", "codex"] as const).map((provider) => (
                  <CheckoutSkill
                    key={JSON.stringify([
                      provider,
                      repo.projectId,
                      repo.checkout,
                      gitlab.origin,
                      connection.expectedRevision,
                      connectionEpoch,
                    ])}
                    body={{
                      expectedRevision: connection.expectedRevision,
                      projectId: repo.projectId,
                      checkout: repo.checkout ?? "",
                      gitlab,
                      token: connection.token,
                      provider,
                    }}
                    selected={repo.defaultSkills[provider] ?? null}
                    onSelect={(skill) =>
                      onChange({
                        ...repo,
                        defaultSkills: { ...repo.defaultSkills, [provider]: skill },
                      })
                    }
                  />
                ))}
              </div>
            )}
          </fieldset>
        </details>
      ))}
    </section>
  );
}
