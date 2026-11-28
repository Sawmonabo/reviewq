import { useEffect, useRef, useState } from "react";
import { ApiResponseError } from "../../api/client";
import { disconnectSettings, readSettings, saveSettings, testConnection } from "../../api/settings";
import type {
  CatalogItem,
  Identity,
  Provider,
  SaveSettingsBoardScope,
  SaveSettingsRepositoryBinding,
  SettingsDraft,
  SettingsView,
  TokenInput,
} from "../../api/generated/contracts/settings";
import { Modal, ModalFrame } from "../../components/Modal";
import { Loading, Notice } from "../../components/PanelState";
import { Wordmark } from "../../components/Wordmark";
import type { Appearance } from "../../hooks/useAppearance";
import { useResource } from "../../hooks/useResource";
import { describeError, readable } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import { AgentPreferences } from "./AgentPreferences";
import { AppearancePage } from "./AppearancePage";
import { ConnectionsPage } from "./ConnectionsPage";
import { Repositories } from "./Repositories";
import {
  boardIncludesRepository,
  clearProviderScope,
  draftOrigins,
  settingsFocus,
  sourceOrigin,
  toggleMembership,
} from "./scope-draft";
import type { ScopeOrigins } from "./scope-draft";
import { SettingsNavigation, SettingsPage, savedPage } from "./SettingsNavigation";
import type { Page } from "./SettingsNavigation";
import { TeamBoards } from "./TeamBoards";
import { TeamReviewers } from "./TeamReviewers";
import { SystemPage } from "./system/SystemPage";
import { useAgentPreferences } from "./useAgentPreferences";
import "./settings.css";

type Editor = {
  readonly snapshot: SettingsView;
  readonly draft: SettingsDraft;
  readonly tokens: Record<Provider, string>;
  readonly scopeOrigins: ScopeOrigins;
};

type Picker = "scope" | "repositories" | "reviewers";
type ScopeReset = { readonly provider: Provider; readonly picker: Picker | undefined };

type Props = {
  readonly onClose: () => void;
  readonly onSave: () => void;
  readonly appearance: Appearance;
  readonly onAppearance: (appearance: Appearance) => void;
};

const settingsMark = (
  <span className="settings-heading-mark" aria-hidden="true">
    <span>/</span>
    <Wordmark className="settings-wordmark" />
  </span>
);

export default function SettingsPane(props: Props) {
  const read = useResource("settings", readSettings, [{ kind: "settings" }]);
  if (read.data) return <SettingsEditor {...props} initial={read.data} />;
  return (
    <UnavailableWorkspace
      {...props}
      pending={read.pending}
      error={read.error}
      onRetry={read.retry}
    />
  );
}

function UnavailableWorkspace({
  onClose,
  appearance,
  onAppearance,
  pending,
  error,
  onRetry,
}: Props & {
  readonly pending: boolean;
  readonly error: string | undefined;
  readonly onRetry: () => void;
}) {
  const [active, setActive] = useState(() => savedPage(savedValue("settings.page")));
  return (
    <ModalFrame
      title="Settings"
      headingMark={settingsMark}
      className="settings-pane"
      onClose={onClose}
    >
      <div className="settings-layout">
        <SettingsNavigation
          active={active}
          onSelect={(page) => {
            setActive(page);
            saveValue("settings.page", page);
          }}
        />
        <div className="modal-body settings-body">
          <SettingsPage page={active} active={active}>
            {active === "appearance" ? (
              <AppearancePage appearance={appearance} onChange={onAppearance} />
            ) : active === "system" ? (
              <SystemPage />
            ) : pending ? (
              <Loading label="Loading workspace settings…" />
            ) : (
              <Notice retry={onRetry}>{error}</Notice>
            )}
          </SettingsPage>
        </div>
      </div>
    </ModalFrame>
  );
}

function SettingsEditor({
  initial,
  onClose,
  onSave,
  appearance,
  onAppearance,
}: Props & { readonly initial: SettingsView }) {
  const [editor, setEditor] = useState(() => fromSaved(initial));
  const [active, setActive] = useState(() => savedPage(savedValue("settings.page")));
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState<Provider>();
  const [confirm, setConfirm] = useState<Provider | "discard">();
  const [verified, setVerified] = useState<Partial<Record<Provider, Identity>>>({});
  const [errors, setErrors] = useState<Partial<Record<Provider, string>>>({});
  const [notice, setNotice] = useState<string>();
  const [credentialEpoch, setCredentialEpoch] = useState<Record<Provider, number>>({
    gitlab: 0,
    jira: 0,
  });
  const [conflict, setConflict] = useState<SettingsView>();
  const [saved, setSaved] = useState<"workspace" | "agents">();
  const [catalog, setCatalog] = useState<Picker>();
  const [scopeReset, setScopeReset] = useState<ScopeReset>();
  const [showScopeErrors, setShowScopeErrors] = useState(false);
  const preferences = useAgentPreferences();
  const tests = useRef<Partial<Record<Provider, AbortController>>>({});
  const errorSummary = useRef<HTMLDivElement>(null);
  const form = useRef<HTMLFormElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const positions = useRef<Partial<Record<Page, number>>>({});
  const pending = useRef(false);
  const { snapshot, draft, tokens } = editor;
  const origins = draftOrigins(draft);
  const wrongScope = {
    gitlab:
      Boolean(draft.repositories.length || draft.additionalReviewerIds.length) &&
      origins.gitlab !== editor.scopeOrigins.gitlab,
    jira: Boolean(draft.boards.length) && origins.jira !== editor.scopeOrigins.jira,
  };
  const gitlab = snapshot.connections.gitlab;
  const savedGitlab =
    gitlab.configured && !wrongScope.gitlab && origins.gitlab === sourceOrigin(gitlab.origin);
  const savedGitlabToken = savedGitlab && !tokens.gitlab;
  const sameGitlabAccount =
    savedGitlabToken ||
    (savedGitlab && Boolean(verified.gitlab && verified.gitlab.id === gitlab.identity?.id));
  const workspaceDirty =
    Boolean(tokens.gitlab || tokens.jira) ||
    JSON.stringify(draft) !== JSON.stringify(fromSaved(snapshot).draft);
  const dirty = workspaceDirty || preferences.dirty;
  const saveArea =
    active === "appearance" || active === "system"
      ? undefined
      : active === "agents"
        ? active
        : "workspace";
  const pageDirty =
    saveArea === "workspace" ? workspaceDirty : saveArea === "agents" && preferences.dirty;
  const footerStatus = busy
    ? "Saving…"
    : pageDirty
      ? "Unsaved changes"
      : saved === saveArea && saved
        ? "Changes saved"
        : dirty
          ? "Unsaved changes on another page"
          : "";
  const newest =
    conflict && BigInt(conflict.settingsRevision) > BigInt(initial.settingsRevision)
      ? conflict
      : initial;
  const stale = BigInt(newest.settingsRevision) > BigInt(snapshot.settingsRevision);

  useEffect(() => {
    const activeTests = tests.current;
    return () => {
      for (const controller of Object.values(activeTests)) controller.abort();
    };
  }, []);

  function select(page: Page) {
    if (page === active) return;
    if (scroller.current) positions.current[active] = scroller.current.scrollTop;
    setActive(page);
    saveValue("settings.page", page);
    requestAnimationFrame(() => {
      if (scroller.current) scroller.current.scrollTop = positions.current[page] ?? 0;
    });
  }

  function openCatalog(picker: Picker) {
    const provider = picker === "scope" ? "jira" : "gitlab";
    if (wrongScope[provider]) askScopeReset(provider, picker);
    else setCatalog(picker);
  }

  function askScopeReset(provider: Provider, picker?: Picker) {
    if (!origins[provider]) {
      select("connections");
      setNotice(
        `Enter a valid ${provider === "jira" ? "Jira site" : "GitLab address"} to configure its scope.`,
      );
      requestAnimationFrame(() => errorSummary.current?.focus());
      return;
    }
    setScopeReset({ provider, picker });
  }

  function edit(update: (current: SettingsDraft) => SettingsDraft, provider?: Provider) {
    setEditor((previous) => ({
      ...previous,
      draft: update(previous.draft),
      scopeOrigins: provider
        ? { ...previous.scopeOrigins, [provider]: draftOrigins(previous.draft)[provider] }
        : previous.scopeOrigins,
    }));
    setSaved(undefined);
  }

  function cancelTests() {
    for (const controller of Object.values(tests.current)) controller.abort();
    setTesting(undefined);
    setVerified({});
    setErrors({});
  }

  function connectionChanged(provider: Provider) {
    setCredentialEpoch((previous) => ({ ...previous, [provider]: previous[provider] + 1 }));
    tests.current[provider]?.abort();
    setTesting((value) => (value === provider ? undefined : value));
    setVerified((previous) => ({ ...previous, [provider]: undefined }));
    setErrors((previous) => ({ ...previous, [provider]: undefined }));
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
    setTesting(provider);
    try {
      const { data } = await testConnection(scoped(provider), controller.signal);
      if (!controller.signal.aborted && data.provider === provider) {
        setVerified((previous) => ({ ...previous, [provider]: data.identity }));
        setErrors((previous) => ({ ...previous, [provider]: undefined }));
      }
    } catch (error) {
      if (!controller.signal.aborted)
        setErrors((previous) => ({ ...previous, [provider]: describeError(error) }));
    } finally {
      if (!controller.signal.aborted) setTesting(undefined);
    }
  }

  function addBoards(items: readonly CatalogItem[]) {
    const boards: SaveSettingsBoardScope[] = [];
    for (const item of items) {
      if (!item.boardType) throw new Error("Jira did not verify the board type.");
      boards.push({
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
      });
    }
    edit(
      (previous) => ({
        ...previous,
        boards: [
          ...previous.boards,
          ...boards.filter(
            (board) => !previous.boards.some((known) => known.boardId === board.boardId),
          ),
        ],
      }),
      "jira",
    );
    if (boards[0]) settingsFocus(`settings-board-${boards[0].boardId}`);
  }

  function addRepositories(items: readonly CatalogItem[]) {
    const repositories: SaveSettingsRepositoryBinding[] = [];
    for (const item of items) {
      if (!item.path) throw new Error("GitLab did not return the repository path.");
      repositories.push({
        projectId: item.id,
        path: item.path,
        boardSelection: "selected",
        checkout: null,
        defaultSkills: {},
      });
    }
    edit(
      (previous) => ({
        ...previous,
        repositories: [
          ...previous.repositories,
          ...repositories.filter(
            (repo) => !previous.repositories.some((known) => known.projectId === repo.projectId),
          ),
        ],
      }),
      "gitlab",
    );
    if (repositories[0]) settingsFocus(`settings-repository-${repositories[0].projectId}`);
  }

  function setMembership(scopeId: string, projectId: string, include: boolean) {
    edit((previous) => ({
      ...previous,
      boards: previous.boards.map((board) =>
        board.scopeId === scopeId
          ? { ...board, projectIds: toggleMembership(board.projectIds, projectId, include) }
          : board,
      ),
    }));
  }

  async function save(disconnect?: Provider) {
    const saveWorkspace = Boolean(disconnect) || saveArea === "workspace";
    if (pending.current || (!disconnect && (!pageDirty || (saveWorkspace && (catalog || stale))))) {
      return;
    }
    setNotice(undefined);
    setSaved(undefined);
    if (!disconnect && saveWorkspace) {
      const invalid = form.current?.querySelector<HTMLInputElement>(
        "#settings-page-connections input:invalid",
      );
      if (invalid) {
        select("connections");
        setNotice(invalid.validationMessage);
        requestAnimationFrame(() => invalid.focus());
        return;
      }
      if (wrongScope.gitlab || wrongScope.jira) {
        askScopeReset(wrongScope.gitlab ? "gitlab" : "jira");
        return;
      }
      setShowScopeErrors(true);
      const missing = draft.boards.find(
        (board) =>
          board.enabled &&
          (!board.reviewStatusIds.length ||
            !draft.repositories.some((repo) => boardIncludesRepository(board, repo))),
      );
      if (missing) {
        if (!missing.reviewStatusIds.length) {
          select("scope");
          setNotice(`${missing.name} needs at least one review status.`);
          settingsFocus(`settings-board-${missing.boardId}`);
        } else {
          select("repositories");
          setNotice(`Include ${missing.name} under Jira boards for at least one repository.`);
          settingsFocus(
            draft.repositories[0]
              ? `settings-repository-${draft.repositories[0].projectId}`
              : "settings-add-repository",
          );
        }
        return;
      }
    }
    pending.current = true;
    setBusy(true);
    try {
      if (disconnect) {
        const { data } = await disconnectSettings({
          provider: disconnect,
          expectedRevision: snapshot.settingsRevision,
        });
        const next = fromSaved(data);
        setEditor({
          ...next,
          scopeOrigins: editor.scopeOrigins,
          draft: { ...draft, [disconnect]: next.draft[disconnect] },
          tokens: { ...tokens, [disconnect]: "" },
        });
        onSave();
      } else if (saveWorkspace) {
        const { data } = await saveSettings({
          expectedRevision: snapshot.settingsRevision,
          draft,
          tokens: { gitlab: tokenInput(tokens.gitlab), jira: tokenInput(tokens.jira) },
        });
        setEditor(fromSaved(data));
        onSave();
      } else {
        await preferences.save();
      }
      if (saveWorkspace) {
        cancelTests();
        setConflict(undefined);
        setShowScopeErrors(false);
      }
      setConfirm(undefined);
      setSaved(saveWorkspace ? "workspace" : "agents");
    } catch (error) {
      setConfirm(undefined);
      setNotice(describeError(error));
      if (
        saveWorkspace &&
        error instanceof ApiResponseError &&
        ["conflict", "stale_revision"].includes(error.code)
      ) {
        try {
          setConflict((await readSettings(AbortSignal.timeout(10000))).data);
        } catch {
          // Keep the draft available for retry.
        }
      }
      requestAnimationFrame(() => errorSummary.current?.focus());
    } finally {
      pending.current = false;
      setBusy(false);
    }
  }

  function close() {
    if (busy) return;
    if (dirty) setConfirm("discard");
    else onClose();
  }

  return (
    <ModalFrame
      title="Settings"
      className="settings-pane"
      headingMark={settingsMark}
      onClose={close}
      busy={busy}
    >
      <div className="settings-layout">
        <SettingsNavigation active={active} onSelect={select} />
        <div className="modal-body settings-body" ref={scroller}>
          <form
            id="workspace-settings"
            ref={form}
            className="stack"
            noValidate
            onSubmit={(event) => {
              event.preventDefault();
              void save();
            }}
          >
            {notice && (
              <div ref={errorSummary} tabIndex={-1} role="alert">
                <Notice>{notice}</Notice>
              </div>
            )}
            {stale && (
              <div className="notice">
                <span>Workspace settings changed in another tab.</span>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => {
                    setEditor(fromSaved(newest));
                    setConflict(undefined);
                    setNotice(undefined);
                    cancelTests();
                    setShowScopeErrors(false);
                  }}
                >
                  {workspaceDirty ? "Discard workspace edits and reload" : "Load saved settings"}
                </button>
              </div>
            )}
            <SettingsPage page="connections" active={active}>
              <ConnectionsPage
                snapshot={snapshot}
                draft={draft}
                tokens={tokens}
                testing={testing}
                verified={verified}
                errors={errors}
                busy={busy}
                onGitLab={(origin) => {
                  connectionChanged("gitlab");
                  edit((current) => ({ ...current, gitlab: { origin } }));
                }}
                onJira={(jira) => {
                  connectionChanged("jira");
                  edit((current) => ({ ...current, jira }));
                }}
                onToken={(provider, value) => {
                  connectionChanged(provider);
                  setEditor((previous) => ({
                    ...previous,
                    tokens: { ...previous.tokens, [provider]: value },
                  }));
                  setSaved(undefined);
                }}
                onCheck={(provider) => void check(provider)}
                onDisconnect={setConfirm}
              />
              {snapshot.chatSetup.state !== "ready" && (
                <Notice>
                  Native chat setup · {readable(snapshot.chatSetup.state)}
                  {snapshot.chatSetup.issues.length > 0 &&
                    ` · ${snapshot.chatSetup.issues.join(" · ")}`}
                </Notice>
              )}
            </SettingsPage>
            <SettingsPage
              page="scope"
              active={active}
              action={
                <button
                  type="button"
                  id="settings-add-board"
                  aria-haspopup="dialog"
                  disabled={busy || stale}
                  onClick={(event) => {
                    event.currentTarget.focus({ preventScroll: true });
                    openCatalog("scope");
                  }}
                >
                  + Add board
                </button>
              }
            >
              <TeamBoards
                key={`team-boards:${snapshot.settingsRevision}:${credentialEpoch.jira}`}
                boards={draft.boards}
                browsing={catalog === "scope"}
                onDismissCatalog={() => setCatalog(undefined)}
                active={active === "scope" && !wrongScope.jira}
                repositories={draft.repositories}
                connection={scoped("jira")}
                connectionEpoch={credentialEpoch.jira}
                disabled={busy || wrongScope.jira}
                showErrors={showScopeErrors}
                onAdd={addBoards}
                onChange={(board) =>
                  edit((previous) => ({
                    ...previous,
                    boards: previous.boards.map((item) =>
                      item.scopeId === board.scopeId ? board : item,
                    ),
                  }))
                }
                onRemove={(scopeId) =>
                  edit((previous) => ({
                    ...previous,
                    boards: previous.boards.filter((board) => board.scopeId !== scopeId),
                  }))
                }
                onRepositories={() => select("repositories")}
              />
              <TeamReviewers
                key={`reviewers:${credentialEpoch.gitlab}`}
                active={active === "scope"}
                browsing={catalog === "reviewers"}
                connection={scoped("gitlab")}
                connectionEpoch={credentialEpoch.gitlab}
                suggestReviewers={savedGitlabToken}
                savedConnection={
                  sameGitlabAccount
                    ? {
                        expectedRevision: snapshot.settingsRevision,
                        provider: "gitlab",
                        connection: { origin: gitlab.origin ?? "" },
                        token: { action: "keep" },
                      }
                    : undefined
                }
                savedIds={snapshot.additionalReviewerIds}
                ids={draft.additionalReviewerIds}
                canLookUp={Boolean(tokens.gitlab && origins.gitlab) || savedGitlabToken}
                disabled={busy || stale}
                onChange={(ids) =>
                  edit(
                    (previous) => ({ ...previous, additionalReviewerIds: ids }),
                    catalog === "reviewers" ? "gitlab" : undefined,
                  )
                }
                onConnections={() => select("connections")}
                onBrowse={() => openCatalog("reviewers")}
                onDismissCatalog={() => setCatalog(undefined)}
              />
            </SettingsPage>
            <SettingsPage
              page="repositories"
              active={active}
              action={
                <button
                  type="button"
                  id="settings-add-repository"
                  aria-haspopup="dialog"
                  disabled={busy || stale}
                  onClick={(event) => {
                    event.currentTarget.focus({ preventScroll: true });
                    openCatalog("repositories");
                  }}
                >
                  + Add repository
                </button>
              }
            >
              <Repositories
                key={`repositories:${snapshot.settingsRevision}:${credentialEpoch.gitlab}`}
                boards={draft.boards}
                repositories={draft.repositories}
                gitlab={draft.gitlab}
                browsing={catalog === "repositories" && active === "repositories"}
                onDismissCatalog={() => setCatalog(undefined)}
                connection={scoped("gitlab")}
                connectionEpoch={credentialEpoch.gitlab}
                disabled={busy || wrongScope.gitlab}
                onAdd={addRepositories}
                onChange={(repo) =>
                  edit((previous) => ({
                    ...previous,
                    repositories: previous.repositories.map((item) =>
                      item.projectId === repo.projectId ? repo : item,
                    ),
                  }))
                }
                onRemove={(projectId) =>
                  edit((previous) => ({
                    ...previous,
                    repositories: previous.repositories.filter(
                      (item) => item.projectId !== projectId,
                    ),
                    boards: previous.boards.map((board) => ({
                      ...board,
                      projectIds: board.projectIds.filter((id) => id !== projectId),
                    })),
                  }))
                }
                onMembership={setMembership}
              />
            </SettingsPage>
            <SettingsPage page="agents" active={active}>
              <AgentPreferences preferences={preferences} disabled={busy} />
            </SettingsPage>
            <SettingsPage page="appearance" active={active}>
              <AppearancePage appearance={appearance} onChange={onAppearance} />
            </SettingsPage>
            <SettingsPage page="system" active={active}>
              {active === "system" && <SystemPage />}
            </SettingsPage>
          </form>
        </div>
      </div>
      <footer>
        <span className="settings-footer-workspace">Workspace settings</span>
        <p role="status">{footerStatus}</p>
        {pageDirty && (
          <button
            type="submit"
            form="workspace-settings"
            className="primary"
            disabled={busy || (saveArea === "workspace" && (Boolean(catalog) || stale))}
          >
            Save changes
          </button>
        )}
        <kbd aria-hidden="true">Esc</kbd>
      </footer>
      {scopeReset && (
        <Modal
          title={
            scopeReset.provider === "gitlab"
              ? "Use a different GitLab address?"
              : "Use a different Jira site?"
          }
          className="settings-confirm"
          onClose={() => setScopeReset(undefined)}
          footer={
            <>
              <button type="button" onClick={() => setScopeReset(undefined)}>
                Cancel
              </button>
              <button
                type="button"
                className="primary"
                onClick={() => {
                  edit(
                    (previous) => clearProviderScope(previous, scopeReset.provider),
                    scopeReset.provider,
                  );
                  setCatalog(scopeReset.picker);
                  setScopeReset(undefined);
                }}
              >
                {scopeReset.provider === "gitlab" ? "Clear GitLab scope" : "Clear Jira boards"}
              </button>
            </>
          }
        >
          <p className="meta">
            {scopeReset.provider === "gitlab"
              ? "This clears tracked repositories and team reviewers from your draft and removes repository links from Jira boards."
              : "This clears tracked boards and their review settings from your draft."}
          </p>
        </Modal>
      )}
      {confirm && (
        <Modal
          title={
            confirm === "discard"
              ? "Discard workspace changes?"
              : `Disconnect ${confirm === "gitlab" ? "GitLab" : "Jira Cloud"}?`
          }
          className="settings-confirm"
          onClose={() => setConfirm(undefined)}
          busy={busy}
          footer={
            <>
              <button type="button" disabled={busy} onClick={() => setConfirm(undefined)}>
                {confirm === "discard" ? "Keep editing" : "Keep connection"}
              </button>
              <button
                type="button"
                className="primary"
                disabled={busy}
                onClick={() => (confirm === "discard" ? onClose() : void save(confirm))}
              >
                {confirm === "discard" ? "Discard changes" : "Confirm disconnect"}
              </button>
            </>
          }
        >
          <p className="meta">
            {confirm === "discard"
              ? "Appearance preferences are already applied."
              : "Future source checks will pause. Existing review history stays accessible."}
          </p>
        </Modal>
      )}
    </ModalFrame>
  );
}

function fromSaved(snapshot: SettingsView): Editor {
  return {
    snapshot,
    tokens: { gitlab: "", jira: "" },
    scopeOrigins: {
      gitlab: sourceOrigin(snapshot.connections.gitlab.origin),
      jira: sourceOrigin(snapshot.connections.jira.site),
    },
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
