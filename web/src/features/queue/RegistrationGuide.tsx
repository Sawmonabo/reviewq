import { useState } from "react";
import type { HostProviderDto } from "../../api/generated/contracts";
import { submits } from "../../lib/submit-key";

export function RegistrationGuide({
  provider,
  webUrl,
  selected,
  advanced,
  nativeId,
  onAdvanced,
  onNativeId,
  onRegister,
  disabled,
}: {
  readonly provider: HostProviderDto;
  readonly webUrl: string | undefined;
  readonly selected: boolean;
  readonly advanced: boolean;
  readonly nativeId: string;
  readonly onAdvanced: (open: boolean) => void;
  readonly onNativeId: (next: string) => void;
  readonly onRegister: () => void;
  readonly disabled: boolean;
}) {
  const [copy, setCopy] = useState<"copied" | "manual">();
  const agent = provider === "claude" ? "Claude Code" : "Codex";
  const command = webUrl ? `${provider === "claude" ? "/reviewq" : "$reviewq"} ${webUrl}` : "";

  async function copyCommand() {
    try {
      await navigator.clipboard.writeText(command);
      setCopy("copied");
    } catch {
      setCopy("manual");
    }
  }
  return (
    <div className="registration-guide stack">
      <label className="field">
        Run inside {agent}
        <textarea
          aria-label="Registration command"
          readOnly
          rows={2}
          value={command}
          placeholder={selected ? "Waiting for the merge request URL" : "Choose a merge request"}
        />
      </label>
      <button
        type="button"
        className="registration-copy"
        aria-live="polite"
        disabled={!command || disabled}
        onClick={() => void copyCommand()}
      >
        {copy === "copied" ? "Command copied" : "Copy command"}
      </button>
      <p className="meta">
        {copy === "manual"
          ? "Select and copy the command above."
          : "Your agent supplies its session identity automatically. The registered session will appear here."}
      </p>
      <details
        className="launch-disclosure"
        open={advanced}
        onToggle={(event) => onAdvanced(event.currentTarget.open)}
      >
        <summary>Attach by session ID</summary>
        <label className="field">
          Exact conversation or thread ID
          <input
            aria-label="Session ID"
            value={nativeId}
            onKeyDown={(event) => {
              if (!event.defaultPrevented && submits(event.nativeEvent)) {
                event.preventDefault();
                onRegister();
              }
            }}
            maxLength={200}
            onChange={(event) => onNativeId(event.currentTarget.value)}
            placeholder="Paste the session ID"
            disabled={disabled || !selected}
          />
        </label>
      </details>
      <p className="meta">Linking a session keeps the current review state.</p>
    </div>
  );
}
