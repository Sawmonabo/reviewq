import type { ReactNode } from "react";
import { Loading, Notice } from "../../components/PanelState";
import type { useAgentPreferences } from "./useAgentPreferences";

export function AgentPreferences({
  preferences,
  disabled,
}: {
  readonly preferences: ReturnType<typeof useAgentPreferences>;
  readonly disabled: boolean;
}) {
  const { recovery, followups, resume, automatic, autoReviewConcurrency, update } = preferences;
  return (
    <section className="settings-section agent-preferences" aria-labelledby="settings-agents">
      <h3 id="settings-agents">Review automation</h3>
      {recovery.error && <Notice retry={recovery.retry}>{recovery.error}</Notice>}
      {followups.error && <Notice retry={followups.retry}>{followups.error}</Notice>}
      {(recovery.pending || followups.pending) && <Loading label="Reading agent preferences…" />}
      <fieldset className="settings-agent-policies" disabled={disabled}>
        {resume !== undefined && (
          <Policy
            title="Resume interrupted Codex reviews"
            description="Continue registered reviews after a temporary usage or capacity limit."
          >
            <Switch
              label="Resume interrupted Codex reviews"
              checked={resume}
              onChange={(value) => update({ recovery: value })}
            />
          </Policy>
        )}
        {automatic !== undefined && (
          <Policy
            title="Review replies to my comments"
            description="Check published threads for replies until you approve the merge request."
          >
            <Switch
              label="Automatically review replies"
              checked={automatic}
              onChange={(value) => update({ automatic: value })}
            />
          </Policy>
        )}
        {autoReviewConcurrency !== undefined && (
          <Policy
            title="Simultaneous follow-ups"
            description="Active reviews finish when you lower this limit."
          >
            <select
              aria-label="Maximum simultaneous follow-ups"
              value={autoReviewConcurrency}
              onChange={(event) =>
                update({ autoReviewConcurrency: Number(event.currentTarget.value) })
              }
            >
              {Array.from({ length: 8 }, (_, index) => (
                <option key={index + 1} value={index + 1}>
                  {index + 1}
                </option>
              ))}
            </select>
          </Policy>
        )}
      </fieldset>
    </section>
  );
}

function Policy({
  title,
  description,
  children,
}: {
  readonly title: string;
  readonly description: string;
  readonly children: ReactNode;
}) {
  return (
    <div className="settings-policy">
      <div>
        <strong>{title}</strong>
        <p className="meta">{description}</p>
      </div>
      {children}
    </div>
  );
}

function Switch({
  label,
  checked,
  onChange,
}: {
  readonly label: string;
  readonly checked: boolean;
  readonly onChange: (checked: boolean) => void;
}) {
  return (
    <label className="settings-switch">
      <input
        className="sr-only"
        type="checkbox"
        role="switch"
        aria-label={label}
        checked={checked}
        onChange={(event) => onChange(event.currentTarget.checked)}
      />
      <span aria-hidden="true" />
    </label>
  );
}
