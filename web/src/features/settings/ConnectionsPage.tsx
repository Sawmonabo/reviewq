import type { Provider, SettingsDraft, SettingsView } from "../../api/generated/contracts/settings";
import { Notice } from "../../components/PanelState";
import gitlabLogo from "./gitlab.svg";
import jiraLogo from "./jira.svg";

export function ConnectionsPage({
  snapshot,
  draft,
  tokens,
  testing,
  verified,
  errors,
  busy,
  onGitLab,
  onJira,
  onToken,
  onCheck,
  onDisconnect,
}: {
  readonly snapshot: SettingsView;
  readonly draft: SettingsDraft;
  readonly tokens: Record<Provider, string>;
  readonly testing: Provider | undefined;
  readonly verified: Partial<Record<Provider, string>>;
  readonly errors: Partial<Record<Provider, string>>;
  readonly busy: boolean;
  readonly onGitLab: (origin: string) => void;
  readonly onJira: (jira: SettingsDraft["jira"]) => void;
  readonly onToken: (provider: Provider, token: string) => void;
  readonly onCheck: (provider: Provider) => void;
  readonly onDisconnect: (provider: Provider) => void;
}) {
  return (
    <div className="settings-connection-grid">
      {(["gitlab", "jira"] as const).map((provider) => {
        const isGitLab = provider === "gitlab";
        const connection = snapshot.connections[provider];
        const changed =
          Boolean(tokens[provider]) ||
          (isGitLab
            ? draft.gitlab.origin !== (snapshot.connections.gitlab.origin ?? "")
            : draft.jira.site !== (snapshot.connections.jira.site ?? "") ||
              draft.jira.email !== (snapshot.connections.jira.email ?? "") ||
              draft.jira.tokenType !== (snapshot.connections.jira.tokenType ?? "unscoped"));
        const label = verified[provider];
        const status = label
          ? "Test passed"
          : changed
            ? "Draft settings"
            : connection.configured
              ? "Configured"
              : "Disconnected";
        const token = (
          <label className="field">
            {isGitLab ? "Personal access token" : "API token"}
            <input
              type="password"
              value={tokens[provider]}
              autoComplete="new-password"
              spellCheck={false}
              onChange={(event) => onToken(provider, event.currentTarget.value)}
              placeholder={connection.configured ? "Saved token · leave blank to keep" : undefined}
            />
          </label>
        );
        return (
          <fieldset className="settings-connection-card" key={provider} disabled={busy}>
            <legend className="sr-only">{isGitLab ? "GitLab" : "Jira Cloud"}</legend>
            <div className="settings-connection-heading">
              <img src={isGitLab ? gitlabLogo : jiraLogo} alt="" width={27} height={27} />
              <div>
                <h3>{isGitLab ? "GitLab" : "Jira Cloud"}</h3>
                <p className="meta">
                  {isGitLab
                    ? "Merge requests, repository scope and review discussions"
                    : "Project boards, review statuses and ticket details"}
                </p>
              </div>
              <span
                className="settings-state"
                data-active={Boolean(label) || (!changed && connection.configured) || undefined}
              >
                {status}
              </span>
            </div>
            <div className="settings-fields">
              {isGitLab ? (
                <>
                  <label className="field">
                    GitLab address
                    <input
                      type="url"
                      value={draft.gitlab.origin}
                      onChange={(event) => onGitLab(event.currentTarget.value)}
                      placeholder="https://gitlab.example.com"
                    />
                  </label>
                  {token}
                </>
              ) : (
                <>
                  <label className="field">
                    Jira site
                    <input
                      type="url"
                      value={draft.jira.site}
                      onChange={(event) =>
                        onJira({ ...draft.jira, site: event.currentTarget.value })
                      }
                      placeholder="https://team.atlassian.net"
                    />
                  </label>
                  <label className="field">
                    Account email
                    <input
                      type="email"
                      value={draft.jira.email}
                      onChange={(event) =>
                        onJira({ ...draft.jira, email: event.currentTarget.value })
                      }
                    />
                  </label>
                  {token}
                  <label className="field">
                    Token type
                    <select
                      value={draft.jira.tokenType}
                      onChange={(event) =>
                        onJira({
                          ...draft.jira,
                          tokenType: event.currentTarget.value === "scoped" ? "scoped" : "unscoped",
                        })
                      }
                    >
                      <option value="unscoped">Unscoped</option>
                      <option value="scoped">Scoped</option>
                    </select>
                  </label>
                </>
              )}
            </div>
            {errors[provider] && <Notice>{errors[provider]}</Notice>}
            <div className="settings-connection-footer">
              <p className={label ? "connection-verified meta" : "meta"} role="status">
                {label
                  ? `Connected as ${label}`
                  : changed
                    ? "Test the connection to check these edits."
                    : connection.identity
                      ? `Account · ${connection.identity.label}`
                      : "Enter an address and token to connect."}
              </p>
              <div className="actions">
                {connection.configured && (
                  <button
                    type="button"
                    className="quiet-button"
                    onClick={() => onDisconnect(provider)}
                  >
                    Disconnect
                  </button>
                )}
                <button
                  type="button"
                  disabled={Boolean(testing)}
                  aria-label={isGitLab ? "Test GitLab" : "Test Jira"}
                  onClick={() => onCheck(provider)}
                >
                  {testing === provider ? "Checking…" : "Test connection"}
                </button>
              </div>
            </div>
          </fieldset>
        );
      })}
    </div>
  );
}
