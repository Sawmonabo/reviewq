import { useState } from "react";
import { Icon } from "../../components/Icon";
import type { Appearance } from "../../hooks/useAppearance";
import "./appearance.css";

const palettes = ["blue", "violet"] as const;
const modes = ["system", "light", "dark"] as const;
const spacings = ["regular", "compact"] as const;

const sampleQueue = [
  ["PLAT-241 · !184", "Keep the selected run on refresh", "Waiting on author", "1 finding"],
  ["PLAT-236 · !178", "Anchor threads to their changed lines", "Approved", "Claude Code"],
  ["PLAT-224 · !169", "Read the next transcript page", "In review", "Codex"],
] as const;

export function AppearancePage({
  appearance,
  onChange,
}: {
  readonly appearance: Appearance;
  readonly onChange: (next: Appearance) => void;
}) {
  const [preview, setPreview] = useState<"queue" | "findings">("queue");
  return (
    <div className="settings-appearance">
      <div className="settings-appearance-controls">
        <fieldset>
          <legend>Color theme</legend>
          <div className="appearance-palettes">
            {palettes.map((palette) => (
              <label className="appearance-palette" key={palette}>
                <input
                  className="sr-only"
                  type="radio"
                  name="palette"
                  value={palette}
                  checked={appearance.palette === palette}
                  onChange={() => onChange({ ...appearance, palette })}
                />
                <span className="palette-thumbnail" data-swatch={palette} aria-hidden="true">
                  <span />
                  <span>
                    <i />
                    <i />
                    <i />
                  </span>
                </span>
                <span className="appearance-caption">
                  {title(palette)}
                  <Icon name="check" size={13} />
                </span>
              </label>
            ))}
          </div>
        </fieldset>
        <AppearanceOptions
          name="Color mode"
          values={modes}
          selected={appearance.mode}
          onChange={(mode) => onChange({ ...appearance, mode })}
        />
        <AppearanceOptions
          name="Queue spacing"
          values={spacings}
          selected={appearance.spacing}
          onChange={(spacing) => onChange({ ...appearance, spacing })}
        />
        <p className="meta">Appearance changes apply immediately.</p>
      </div>
      <section className="appearance-preview" aria-label="Review preview">
        <header>
          <strong>Live preview</strong>
          <div className="appearance-preview-tabs" role="group" aria-label="Preview screen">
            {(["queue", "findings"] as const).map((view) => (
              <button
                type="button"
                key={title(view)}
                aria-pressed={preview === view}
                onClick={() => setPreview(view)}
              >
                {title(view)}
              </button>
            ))}
          </div>
        </header>
        <div className="appearance-preview-heading">
          <p className="meta">{preview === "queue" ? "Platform engineering" : "PLAT-241 · !184"}</p>
          <h3>{preview === "queue" ? "Review queue" : "Findings"}</h3>
        </div>
        {preview === "queue" ? (
          sampleQueue.map(([ticket, title, status, detail], index) => (
            <article
              className="appearance-preview-row"
              data-selected={index === 0 || undefined}
              key={ticket}
            >
              <small>{ticket}</small>
              <h4>{title}</h4>
              <p className="meta">
                <span className="appearance-preview-status" data-status={index}>
                  {status}
                </span>{" "}
                {detail}
              </p>
            </article>
          ))
        ) : (
          <article className="appearance-preview-finding">
            <p className="meta">
              <span className="appearance-preview-status" data-status="0">
                Medium
              </span>{" "}
              F-001
            </p>
            <h4>Preserve the selected run on refresh</h4>
            <p>A late response can replace the run the reviewer just selected.</p>
            <code>queue/selection.ts · 42</code>
            <pre>
              <del>− select(firstRun)</del>
              <ins>+ select(confirmedRun)</ins>
            </pre>
          </article>
        )}
      </section>
    </div>
  );
}

function AppearanceOptions<T extends string>({
  name,
  values,
  selected,
  onChange,
}: {
  readonly name: string;
  readonly values: readonly T[];
  readonly selected: T;
  readonly onChange: (value: T) => void;
}) {
  return (
    <fieldset>
      <legend>{name}</legend>
      <div className="appearance-options">
        {values.map((value) => (
          <label key={value}>
            <input
              className="sr-only"
              type="radio"
              name={name}
              value={value}
              checked={selected === value}
              onChange={() => onChange(value)}
            />
            <span>{title(value)}</span>
            <Icon name="check" size={12} />
          </label>
        ))}
      </div>
    </fieldset>
  );
}

function title(value: string) {
  return value.charAt(0).toUpperCase() + value.slice(1);
}
