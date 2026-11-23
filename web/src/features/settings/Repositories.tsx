import type {
  CatalogItem,
  GitlabDraft,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
} from "../../api/generated/contracts/settings";
import { Icon } from "../../components/Icon";
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
  browsing,
  onDismissCatalog,
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
  readonly browsing: boolean;
  readonly onDismissCatalog: () => void;
  readonly onAdd: (item: CatalogItem) => void;
  readonly onChange: (repo: SaveSettingsRepositoryBinding) => void;
  readonly onRemove: (id: string) => void;
  readonly onMembership: (scopeId: string, projectId: string, include: boolean) => void;
}) {
  return (
    <section className="settings-section stack" aria-labelledby="settings-repositories">
      {browsing && (
        <CatalogPicker
          kind="projects"
          label="Browse repositories"
          inputId="settings-repository-search"
          scope={connection}
          connectionEpoch={connectionEpoch}
          disabled={disabled}
          unavailable={repositories.map((repo) => repo.projectId)}
          embedded
          onDismiss={onDismissCatalog}
          onAdd={onAdd}
        />
      )}
      <div className="settings-inline">
        <h3 id="settings-repositories">Tracked repositories</h3>
        <span className="meta">{repositories.length}</span>
      </div>
      {repositories.map((repo, index) => (
        <details className="settings-selection" key={repo.projectId} open={index === 0}>
          <summary id={`settings-repository-${repo.projectId}`}>
            <span className="settings-resource-icon">
              <Icon name="folder" />
            </span>
            <span className="settings-resource-name">
              <strong>{repo.path}</strong>
              <small>
                Project #{repo.projectId} · {repo.checkout || "Checkout unset"}
              </small>
            </span>
            <Icon name="chevronRight" size={14} />
          </summary>
          <fieldset className="settings-selection-body stack" disabled={disabled}>
            <label className="field">
              Jira boards
              <select
                aria-label={`Jira boards for ${repo.path}`}
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
            {repo.boardSelection === "selected" && boards.length > 0 && (
              <fieldset className="settings-choice-group settings-choices">
                <legend>Included boards</legend>
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
              </fieldset>
            )}
            {!boards.some((board) => board.enabled && boardIncludesRepository(board, repo)) && (
              <p className="meta">
                {boards.length
                  ? "Select a team board to include this repository in your review queue."
                  : "Add a team board under Review scope to include this repository in your review queue."}
              </p>
            )}
            <label className="field">
              Local review checkout
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
            <button
              type="button"
              className="quiet-button settings-remove"
              onClick={() => {
                onRemove(repo.projectId);
                settingsFocus("settings-add-repository");
              }}
            >
              Remove repository
            </button>
          </fieldset>
        </details>
      ))}
      {!repositories.length && (
        <p className="meta">Add a GitLab project to configure its checkout.</p>
      )}
    </section>
  );
}
