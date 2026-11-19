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
import { AgentPreferences } from "./AgentPreferences";
import { Repositories } from "./Repositories";
import { boardIncludesRepository, settingsFocus, toggleMembership } from "./scope-draft";
import { TeamBoards } from "./TeamBoards";
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
  const [showScopeErrors, setShowScopeErrors] = useState(false);
  const [resolvingBoard, setResolvingBoard] = useState(false);
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
    changeDraft(() => next);
  }
  function changeDraft(update: (draft: SettingsDraft) => SettingsDraft) {
    setEditor((previous) => ({ ...previous, draft: update(previous.draft) }));
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
    if (!item.boardType) return;
    const board: SaveSettingsBoardScope = {
      boardId: item.id,
      scopeId: crypto.randomUUID(),
      name: item.label,
      type: item.boardType,
      enabled: true,
      jiraProjectKeys: [],
      projectIds: [],
      reviewStatusIds: [],
      storyPointsField: null,
      teamFilterId: null,
    };
    changeDraft((previous) =>
      previous.boards.some((known) => known.boardId === board.boardId)
        ? previous
        : { ...previous, boards: [...previous.boards, board] },
    );
    settingsFocus(`settings-board-${item.id}`);
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
    settingsFocus(`settings-repository-${item.id}`);
  }
  function patchBoard(board: SaveSettingsBoardScope) {
    changeDraft((previous) => ({
      ...previous,
      boards: previous.boards.map((item) => (item.scopeId === board.scopeId ? board : item)),
    }));
  }
  function patchRepo(repo: SaveSettingsRepositoryBinding) {
    changeDraft((previous) => ({
      ...previous,
      repositories: previous.repositories.map((item) =>
        item.projectId === repo.projectId ? repo : item,
      ),
    }));
  }
  function setBoardMembership(scopeId: string, projectId: string, include: boolean) {
    changeDraft((previous) => ({
      ...previous,
      boards: previous.boards.map((board) =>
        board.scopeId === scopeId
          ? { ...board, projectIds: toggleMembership(board.projectIds, projectId, include) }
          : board,
      ),
    }));
  }
  function removeRepo(projectId: string) {
    changeDraft((previous) => ({
      ...previous,
      repositories: previous.repositories.filter((item) => item.projectId !== projectId),
      boards: previous.boards.map((board) => ({
        ...board,
        projectIds: board.projectIds.filter((id) => id !== projectId),
      })),
    }));
  }

  async function commit(action: "save" | "disconnect") {
    if (busy || (action === "save" && resolvingBoard)) return;
    setNotice(undefined);
    setSaved(false);
    const reviewerIds = splitList(reviewers);
    if (action === "save") {
      setShowScopeErrors(true);
      const missing = draft.boards.find(
        (board) =>
          board.enabled &&
          (!board.reviewStatusIds.length ||
            !draft.repositories.some((repo) => boardIncludesRepository(board, repo))),
      );
      const badReviewer = reviewerIds.some(
        (id) => !/^[1-9]\d{0,18}$/.test(id) || BigInt(id) > 9_223_372_036_854_775_807n,
      );
      if (missing || badReviewer) {
        setNotice(
          missing
            ? `${missing.name} needs review statuses and at least one repository before enabling.`
            : "Additional reviewers must be GitLab user IDs.",
        );
        settingsFocus(missing ? `settings-board-${missing.boardId}` : "settings-reviewer-ids");
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
      setShowScopeErrors(false);
      setSaved(true);
      onSave();
      if (action === "save" && result.data.chatSetup.state === "ready") onClose();
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
            disabled={busy || resolvingBoard || Boolean(conflict)}
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
                  setShowScopeErrors(false);
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
          <TeamBoards
            key={`team-boards:${snapshot.settingsRevision}:${credentialEpoch.jira}`}
            boards={draft.boards}
            repositories={draft.repositories}
            connection={scoped("jira")}
            connectionEpoch={credentialEpoch.jira}
            disabled={busy}
            showErrors={showScopeErrors}
            onResolving={setResolvingBoard}
            onAdd={addBoard}
            onChange={patchBoard}
            onRemove={(scopeId) =>
              changeDraft((previous) => ({
                ...previous,
                boards: previous.boards.filter((board) => board.scopeId !== scopeId),
              }))
            }
            onMembership={setBoardMembership}
          />
          <Repositories
            key={`repositories:${snapshot.settingsRevision}`}
            boards={draft.boards}
            repositories={draft.repositories}
            gitlab={draft.gitlab}
            connection={scoped("gitlab")}
            connectionEpoch={credentialEpoch.gitlab}
            disabled={busy}
            onAdd={addRepo}
            onChange={patchRepo}
            onRemove={removeRepo}
            onMembership={setBoardMembership}
          />
          <details className="settings-advanced settings-reviewers">
            <summary>Additional reviewers</summary>
            <label className="field">
              GitLab user IDs
              <input
                id="settings-reviewer-ids"
                value={reviewers}
                disabled={busy}
                placeholder="Optional"
                onChange={(event) => {
                  setReviewers(event.currentTarget.value);
                  setSaved(false);
                }}
              />
            </label>
            <p className="meta">Separate user IDs with commas or spaces.</p>
          </details>
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
        .split(/[\s,]+/)
        .map((item) => item.trim())
        .filter(Boolean),
    ),
  ];
}
