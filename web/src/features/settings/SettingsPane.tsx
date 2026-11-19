import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { ApiResponseError } from "../../api/client";
import { disconnectSettings, readSettings, saveSettings, testConnection } from "../../api/settings";
import type {
  CatalogItem,
  Provider,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
  SettingsDraft,
  SettingsView,
  TokenInput,
} from "../../api/generated/contracts/settings";
import { Modal } from "../../components/Modal";
import { Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { describeError, readable } from "../../lib/presentation";
import { CheckoutSkill } from "./CheckoutSkill";
import { AgentPreferences } from "./AgentPreferences";
import { CatalogPicker } from "./CatalogPicker";
import "./settings.css";

type Editor = {
  readonly snapshot: SettingsView;
  readonly draft: SettingsDraft;
  readonly tokens: Record<Provider, string>;
};
type Verified = { readonly identity: string };

export default function SettingsPane({
  onClose,
  onSave,
  displayOptions,
}: {
  readonly onClose: () => void;
  readonly onSave: () => void;
  readonly displayOptions?: ReactNode;
}) {
  const read = useResource("settings", readSettings);
  return read.data ? (
    <SettingsEditor
      initial={read.data}
      onClose={onClose}
      onSave={onSave}
      displayOptions={displayOptions}
    />
  ) : (
    <Modal title="Settings" eyebrow="Your workspace" className="settings-pane" onClose={onClose}>
      {read.error ? (
        <Notice retry={read.retry}>{read.error}</Notice>
      ) : (
        <Loading label="Loading workspace settings…" />
      )}
    </Modal>
  );
}

function SettingsEditor({
  initial,
  onClose,
  onSave,
  displayOptions,
}: {
  readonly initial: SettingsView;
  readonly onClose: () => void;
  readonly onSave: () => void;
  readonly displayOptions?: ReactNode;
}) {
  const [editor, setEditor] = useState(() => fromSaved(initial));
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState<Provider>();
  const [confirmation, setConfirmation] = useState<Provider>();
  const [verified, setVerified] = useState<Partial<Record<Provider, Verified>>>({});
  const [errors, setErrors] = useState<Partial<Record<Provider, string>>>({});
  const [notice, setNotice] = useState<string>();
  const [credentialEpoch, setCredentialEpoch] = useState<Record<Provider, number>>({
    gitlab: 0,
    jira: 0,
  });
  const [conflict, setConflict] = useState<SettingsView>();
  const [saved, setSaved] = useState(false);
  const [reviewers, setReviewers] = useState(initial.additionalReviewerIds.join(", "));
  const tests = useRef<Partial<Record<Provider, AbortController>>>({});
  const errorSummary = useRef<HTMLDivElement>(null);
  const { snapshot, draft, tokens } = editor;
  useEffect(() => {
    const active = tests.current;
    return () => {
      for (const controller of Object.values(active)) controller.abort();
    };
  }, []);

  function edit(next: SettingsDraft) {
    setEditor((previous) => ({ ...previous, draft: next }));
    setSaved(false);
  }
  function connectionChanged(provider: Provider) {
    setCredentialEpoch((previous) => ({ ...previous, [provider]: previous[provider] + 1 }));
    tests.current[provider]?.abort();
    setTesting((value) => (value === provider ? undefined : value));
    setVerified((previous) => ({ ...previous, [provider]: undefined }));
    setErrors((previous) => ({ ...previous, [provider]: undefined }));
  }
  function writeToken(provider: Provider, value: string) {
    connectionChanged(provider);
    setEditor((previous) => ({ ...previous, tokens: { ...previous.tokens, [provider]: value } }));
    setSaved(false);
  }
  function scoped(provider: Provider) {
    return {
      expectedRevision: snapshot.settingsRevision,
      provider,
      connection: draft[provider],
      token: tokenInput(tokens[provider]),
    };
  }

  async function check(provider: Provider) {
    tests.current[provider]?.abort();
    const controller = new AbortController();
    tests.current[provider] = controller;
    const body = scoped(provider);
    setTesting(provider);
    try {
      const { data } = await testConnection(body, controller.signal);
      if (!controller.signal.aborted && data.provider === provider) {
        setVerified((previous) => ({ ...previous, [provider]: { identity: data.identity.label } }));
        setErrors((previous) => ({ ...previous, [provider]: undefined }));
      }
    } catch (error) {
      if (!controller.signal.aborted)
        setErrors((previous) => ({ ...previous, [provider]: describeError(error) }));
    } finally {
      if (!controller.signal.aborted) setTesting(undefined);
    }
  }

  function addBoard(item: CatalogItem) {
    if (!item.boardType) {
      setNotice("Jira did not return a supported board type.");
      return;
    }
    if (draft.boards.some((board) => board.boardId === item.id)) return;
    edit({
      ...draft,
      boards: [
        ...draft.boards,
        {
          boardId: item.id,
          scopeId: crypto.randomUUID(),
          name: item.label,
          type: item.boardType,
          enabled: false,
          jiraProjectKeys: [],
          projectIds: [],
          reviewStatusIds: [],
          storyPointsField: item.estimateField,
          teamFilterId: null,
        },
      ],
    });
  }
  function addRepo(item: CatalogItem) {
    if (!item.path || draft.repositories.some((repo) => repo.projectId === item.id)) return;
    edit({
      ...draft,
      repositories: [
        ...draft.repositories,
        {
          projectId: item.id,
          path: item.path,
          boardSelection: "selected",
          checkout: null,
          defaultSkills: {},
        },
      ],
    });
    requestAnimationFrame(() => document.getElementById(`checkout-${item.id}`)?.focus());
  }
  function patchBoard(board: SaveSettingsBoardScope) {
    edit({
      ...draft,
      boards: draft.boards.map((item) => (item.scopeId === board.scopeId ? board : item)),
    });
  }
  function patchRepo(repo: SaveSettingsRepositoryBinding) {
    edit({
      ...draft,
      repositories: draft.repositories.map((item) =>
        item.projectId === repo.projectId ? repo : item,
      ),
    });
  }

  async function commit(action: "save" | "disconnect") {
    if (busy) return;
    setNotice(undefined);
    setSaved(false);
    const reviewerIds = splitList(reviewers);
    if (action === "save") {
      const missing = draft.boards.find(
        (board) => board.enabled && (!board.reviewStatusIds.length || !board.projectIds.length),
      );
      if (missing || reviewerIds.some((id) => !/^[1-9]\d{0,18}$/.test(id))) {
        setNotice(
          missing
            ? `${missing.name} needs review statuses and at least one repository before enabling.`
            : "Additional reviewers must be GitLab user IDs.",
        );
        requestAnimationFrame(() => errorSummary.current?.focus());
        return;
      }
    }
    setBusy(true);
    try {
      const result =
        action === "disconnect" && confirmation
          ? await disconnectSettings({
              provider: confirmation,
              expectedRevision: snapshot.settingsRevision,
            })
          : await saveSettings({
              expectedRevision: snapshot.settingsRevision,
              draft: { ...draft, additionalReviewerIds: reviewerIds },
              tokens: { gitlab: tokenInput(tokens.gitlab), jira: tokenInput(tokens.jira) },
            });
      setEditor(fromSaved(result.data));
      setReviewers(result.data.additionalReviewerIds.join(", "));
      setVerified({});
      setErrors({});
      setConflict(undefined);
      setConfirmation(undefined);
      setSaved(true);
      onSave();
    } catch (error) {
      setNotice(describeError(error));
      if (
        error instanceof ApiResponseError &&
        ["conflict", "stale_revision"].includes(error.code)
      ) {
        try {
          setConflict((await readSettings(AbortSignal.timeout(10000))).data);
        } catch {
          /* keep dirty form */
        }
      }
      requestAnimationFrame(() => errorSummary.current?.focus());
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal
      title="Settings"
      eyebrow="Your workspace"
      className="settings-pane"
      onClose={onClose}
      busy={busy}
      footer={
        <>
          <button type="button" disabled={busy} onClick={onClose}>
            {saved ? "Done" : "Cancel"}
          </button>
          <button
            type="submit"
            form="workspace-settings"
            className="primary"
            disabled={busy || Boolean(conflict)}
          >
            {busy ? "Saving…" : "Save changes"}
          </button>
        </>
      }
    >
      <div className="settings-body">
        <AgentPreferences />
        <form
          className="stack"
          id="workspace-settings"
          onSubmit={(event) => {
            event.preventDefault();
            void commit("save");
          }}
        >
          {notice && (
            <div ref={errorSummary} tabIndex={-1}>
              <Notice>{notice}</Notice>
            </div>
          )}
          {conflict && (
            <div className="notice">
              <span>
                Settings changed in another tab. Saved revision {conflict.settingsRevision}.
                Reloading the saved version discards this editor’s changes.
              </span>
              <button
                type="button"
                onClick={() => {
                  setEditor(fromSaved(conflict));
                  setReviewers(conflict.additionalReviewerIds.join(", "));
                  setConflict(undefined);
                  setNotice(undefined);
                  setVerified({});
                }}
              >
                Load saved revision
              </button>
            </div>
          )}
          {saved && (
            <Notice>Saved workspace settings · revision {snapshot.settingsRevision}</Notice>
          )}
          {snapshot.chatSetup.state !== "ready" && (
            <Notice>
              Native chat setup · {readable(snapshot.chatSetup.state)}
              {snapshot.chatSetup.issues.length > 0 &&
                ` · ${snapshot.chatSetup.issues.join(" · ")}`}
            </Notice>
          )}
          <section className="settings-section stack" aria-labelledby="settings-connections">
            <div className="spread">
              <h3 id="settings-connections">Connections</h3>
              {snapshot.mode !== "connected" && (
                <span className="badge">{readable(snapshot.mode)}</span>
              )}
            </div>
            <div className="connection-grid">
              {(["gitlab", "jira"] as const).map((provider) => {
                const connection = snapshot.connections[provider];
                const confirmationResult = verified[provider];
                const verifiedLabel = confirmationResult?.identity;
                return (
                  <fieldset className="connection-fields" key={provider} disabled={busy}>
                    <legend className="sr-only">
                      {provider === "gitlab" ? "GitLab" : "Jira Cloud"}
                    </legend>
                    <div className="connection-heading">
                      <h4>{provider === "gitlab" ? "GitLab" : "Jira Cloud"}</h4>
                      {connection.configured && (
                        <button
                          type="button"
                          className="quiet-button"
                          onClick={() => setConfirmation(provider)}
                        >
                          Disconnect
                        </button>
                      )}
                    </div>
                    {provider === "gitlab" ? (
                      <label className="field">
                        GitLab address
                        <input
                          type="url"
                          value={draft.gitlab.origin}
                          onChange={(event) => {
                            connectionChanged(provider);
                            edit({ ...draft, gitlab: { origin: event.currentTarget.value } });
                          }}
                          placeholder="https://gitlab.example.com"
                        />
                      </label>
                    ) : (
                      <>
                        <label className="field">
                          Jira site
                          <input
                            type="url"
                            value={draft.jira.site}
                            onChange={(event) => {
                              connectionChanged(provider);
                              edit({
                                ...draft,
                                jira: { ...draft.jira, site: event.currentTarget.value },
                              });
                            }}
                            placeholder="https://team.atlassian.net"
                          />
                        </label>
                        <label className="field">
                          Account email
                          <input
                            type="email"
                            value={draft.jira.email}
                            onChange={(event) => {
                              connectionChanged(provider);
                              edit({
                                ...draft,
                                jira: { ...draft.jira, email: event.currentTarget.value },
                              });
                            }}
                          />
                        </label>
                      </>
                    )}
                    <div className={provider === "jira" ? "jira-token-fields" : undefined}>
                      <label className="field">
                        {provider === "gitlab" ? "Personal access token" : "API token"}
                        <input
                          type="password"
                          value={tokens[provider]}
                          autoComplete="new-password"
                          spellCheck={false}
                          onChange={(event) => writeToken(provider, event.currentTarget.value)}
                          placeholder={
                            connection.configured ? "Saved token · leave blank to keep" : ""
                          }
                        />
                      </label>
                      {provider === "jira" && (
                        <label className="field">
                          Token type
                          <select
                            value={draft.jira.tokenType}
                            onChange={(event) => {
                              connectionChanged(provider);
                              edit({
                                ...draft,
                                jira: {
                                  ...draft.jira,
                                  tokenType:
                                    event.currentTarget.value === "scoped" ? "scoped" : "unscoped",
                                },
                              });
                            }}
                          >
                            <option value="unscoped">Unscoped</option>
                            <option value="scoped">Scoped</option>
                          </select>
                        </label>
                      )}
                    </div>
                    <button
                      type="button"
                      disabled={Boolean(testing)}
                      onClick={() => void check(provider)}
                    >
                      {testing === provider
                        ? "Checking…"
                        : provider === "gitlab"
                          ? "Test GitLab"
                          : "Test Jira"}
                    </button>
                    {errors[provider] && <Notice>{errors[provider]}</Notice>}
                    {verifiedLabel && (
                      <p className="connection-verified" role="status">
                        Connected as {verifiedLabel}
                      </p>
                    )}
                  </fieldset>
                );
              })}
            </div>
            {confirmation && (
              <div className="notice">
                Disconnect {confirmation === "gitlab" ? "GitLab" : "Jira"}? Future source reads
                pause. Existing review history stays accessible.
                <button type="button" disabled={busy} onClick={() => void commit("disconnect")}>
                  Confirm disconnect
                </button>
                <button type="button" disabled={busy} onClick={() => setConfirmation(undefined)}>
                  Keep connection
                </button>
              </div>
            )}
          </section>
          <section className="settings-section stack" aria-labelledby="team-boards">
            <h3 id="team-boards">Team boards</h3>
            <CatalogPicker
              connectionEpoch={credentialEpoch.jira}
              scope={scoped("jira")}
              kind="boards"
              label="Browse Jira boards"
              unavailable={draft.boards.map((board) => board.boardId)}
              onAdd={addBoard}
            />
            {draft.boards.map((board) => (
              <details className="surface-card stack" key={board.scopeId}>
                <summary>
                  <strong>{board.name}</strong> ·{" "}
                  <span className="meta">
                    {readable(board.type)} · {board.enabled ? "Enabled" : "Paused"}
                  </span>
                </summary>
                <div className="stack settings-editor">
                  <label>
                    <input
                      type="checkbox"
                      checked={board.enabled}
                      onChange={(event) =>
                        patchBoard({ ...board, enabled: event.currentTarget.checked })
                      }
                    />{" "}
                    Enable discovery
                  </label>
                  <div className="fields">
                    <label className="field">
                      Jira project keys
                      <input
                        value={board.jiraProjectKeys.join(",")}
                        onChange={(event) =>
                          patchBoard({
                            ...board,
                            jiraProjectKeys: event.currentTarget.value.split(","),
                          })
                        }
                      />
                    </label>
                    <label className="field">
                      Estimate field
                      <input
                        value={board.storyPointsField ?? ""}
                        onChange={(event) =>
                          patchBoard({
                            ...board,
                            storyPointsField: event.currentTarget.value || null,
                          })
                        }
                      />
                    </label>
                    <label className="field">
                      Saved filter ID
                      <input
                        value={board.teamFilterId ?? ""}
                        onChange={(event) =>
                          patchBoard({ ...board, teamFilterId: event.currentTarget.value || null })
                        }
                      />
                    </label>
                  </div>
                  <CatalogPicker
                    connectionEpoch={credentialEpoch.jira}
                    scope={scoped("jira")}
                    kind="filters"
                    label="Browse saved filters"
                    unavailable={board.teamFilterId ? [board.teamFilterId] : []}
                    onAdd={(item) => patchBoard({ ...board, teamFilterId: item.id })}
                  />
                  <CatalogPicker
                    connectionEpoch={credentialEpoch.jira}
                    scope={{ ...scoped("jira"), boardId: board.boardId }}
                    kind="statuses"
                    label="Browse review statuses"
                    unavailable={board.reviewStatusIds}
                    onAdd={(item) =>
                      patchBoard({ ...board, reviewStatusIds: [...board.reviewStatusIds, item.id] })
                    }
                  />
                  <div className="actions">
                    {board.reviewStatusIds.map((id) => (
                      <button
                        type="button"
                        key={id}
                        aria-label={`Remove status ${id}`}
                        onClick={() =>
                          patchBoard({
                            ...board,
                            reviewStatusIds: board.reviewStatusIds.filter((item) => item !== id),
                          })
                        }
                      >
                        Status {id} ×
                      </button>
                    ))}
                  </div>
                  <div className="stack">
                    <strong>Repositories in this board</strong>
                    {draft.repositories.map((repo) => (
                      <label key={repo.projectId}>
                        <input
                          type="checkbox"
                          checked={board.projectIds.includes(repo.projectId)}
                          onChange={(event) =>
                            patchBoard({
                              ...board,
                              projectIds: event.currentTarget.checked
                                ? [...board.projectIds, repo.projectId]
                                : board.projectIds.filter((id) => id !== repo.projectId),
                            })
                          }
                        />{" "}
                        {repo.path}
                      </label>
                    ))}
                  </div>
                  <button
                    type="button"
                    onClick={() =>
                      edit({
                        ...draft,
                        boards: draft.boards.filter((item) => item.scopeId !== board.scopeId),
                      })
                    }
                  >
                    Remove board
                  </button>
                </div>
              </details>
            ))}
          </section>
          <section className="settings-section stack" aria-labelledby="settings-repositories">
            <h3 id="settings-repositories">Repositories &amp; agents</h3>
            <CatalogPicker
              connectionEpoch={credentialEpoch.gitlab}
              scope={scoped("gitlab")}
              kind="projects"
              label="Browse repositories"
              unavailable={draft.repositories.map((repo) => repo.projectId)}
              onAdd={addRepo}
            />
            {draft.repositories.map((repo) => (
              <div className="surface-card stack" key={repo.projectId}>
                <div className="spread">
                  <strong>{repo.path}</strong>
                  <button
                    type="button"
                    onClick={() =>
                      edit({
                        ...draft,
                        repositories: draft.repositories.filter(
                          (item) => item.projectId !== repo.projectId,
                        ),
                        boards: draft.boards.map((board) => ({
                          ...board,
                          projectIds: board.projectIds.filter((id) => id !== repo.projectId),
                        })),
                      })
                    }
                  >
                    Remove repository
                  </button>
                </div>
                <div className="fields">
                  <label className="field">
                    Local review checkout
                    <input
                      id={`checkout-${repo.projectId}`}
                      value={repo.checkout ?? ""}
                      onChange={(event) =>
                        patchRepo({
                          ...repo,
                          checkout: event.currentTarget.value || null,
                          defaultSkills: {},
                        })
                      }
                      placeholder="/home/me/repos/project"
                    />
                  </label>
                  <label className="field">
                    Board access
                    <select
                      value={repo.boardSelection}
                      onChange={(event) =>
                        patchRepo({
                          ...repo,
                          boardSelection: event.currentTarget.value === "any" ? "any" : "selected",
                        })
                      }
                    >
                      <option value="selected">Selected boards</option>
                      <option value="any">Any tracked board</option>
                    </select>
                  </label>
                </div>
                {repo.checkout && (
                  <div className="fields">
                    {(["codex", "claude"] as const).map((provider) => (
                      <CheckoutSkill
                        key={JSON.stringify([
                          provider,
                          repo.projectId,
                          repo.checkout,
                          draft.gitlab.origin,
                          snapshot.settingsRevision,
                          credentialEpoch.gitlab,
                        ])}
                        body={{
                          expectedRevision: snapshot.settingsRevision,
                          projectId: repo.projectId,
                          checkout: repo.checkout ?? "",
                          gitlab: draft.gitlab,
                          token: tokenInput(tokens.gitlab),
                          provider,
                        }}
                        selected={repo.defaultSkills[provider] ?? null}
                        onSelect={(skill) =>
                          patchRepo({
                            ...repo,
                            defaultSkills: { ...repo.defaultSkills, [provider]: skill },
                          })
                        }
                      />
                    ))}
                  </div>
                )}
              </div>
            ))}
          </section>
          <section className="settings-section stack" aria-labelledby="additional-reviewers">
            <h3 id="additional-reviewers">Additional reviewers</h3>
            <label className="field">
              GitLab user IDs separated by commas
              <input
                value={reviewers}
                onChange={(event) => setReviewers(event.currentTarget.value)}
              />
            </label>
          </section>
        </form>
        {displayOptions}
      </div>
    </Modal>
  );
}

function fromSaved(snapshot: SettingsView): Editor {
  return {
    snapshot,
    tokens: { gitlab: "", jira: "" },
    draft: {
      gitlab: { origin: snapshot.connections.gitlab.origin ?? "" },
      jira: {
        site: snapshot.connections.jira.site ?? "",
        email: snapshot.connections.jira.email ?? "",
        tokenType: snapshot.connections.jira.tokenType ?? "unscoped",
      },
      additionalReviewerIds: snapshot.additionalReviewerIds,
      boards: snapshot.boards,
      repositories: snapshot.repositories,
    },
  };
}

function tokenInput(value: string): TokenInput {
  return value ? { action: "replace", value } : { action: "keep" };
}

function splitList(value: string) {
  return [
    ...new Set(
      value
        .split(",")
        .map((item) => item.trim())
        .filter(Boolean),
    ),
  ];
}
