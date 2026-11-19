import { useState } from "react";
import type { HostProviderDto } from "../../api/generated/contracts";
import { Notice } from "../../components/PanelState";
import { safeUrl } from "../../lib/presentation";

export function RegistrationGuide({
  provider,
  webUrl,
  advanced,
  nativeId,
  onAdvanced,
  onNativeId,
  disabled,
}: {
  readonly provider: HostProviderDto;
  readonly webUrl: string | undefined;
  readonly advanced: boolean;
  readonly nativeId: string;
  readonly onAdvanced: (open: boolean) => void;
  readonly onNativeId: (next: string) => void;
  readonly disabled: boolean;
}) {
  const [notice, setNotice] = useState<string>();
  const agent = provider === "claude" ? "Claude Code" : "Codex";
  const target = safeUrl(webUrl);
  const command = `${provider === "claude" ? "/reviewq" : "$reviewq"} ${target ?? "<merge request URL>"}`;
  return (
    <div className="registration-guide stack">
      <label className="field">
        Run inside {agent}
        <textarea aria-label="Registration command" readOnly rows={2} value={command} />
      </label>
      <div className="actions">
        <button
          type="button"
          disabled={!target || disabled}
          onClick={() => {
            void navigator.clipboard.writeText(command).then(
              () => setNotice("Registration command copied."),
              () => setNotice("Select and copy the command above."),
            );
          }}
        >
          Copy command
        </button>
      </div>
      <p className="meta">
        Your agent supplies its session identity automatically. The registered session will appear
        here.
      </p>
      {notice && <Notice>{notice}</Notice>}
      <details
        className="launch-disclosure"
        open={advanced}
        onToggle={(event) => onAdvanced(event.currentTarget.open)}
      >
        <summary>Attach by session ID</summary>
        <label className="field">
          Exact conversation or thread ID
          <input
            value={nativeId}
            maxLength={200}
            onChange={(event) => onNativeId(event.currentTarget.value)}
            placeholder="Paste the session ID"
            disabled={disabled}
          />
        </label>
      </details>
      <p className="meta">Linking a session keeps the current review state.</p>
    </div>
  );
}
