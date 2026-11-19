import { useState } from "react";
import type { LaunchTarget } from "./types";
import { Loading, Notice } from "../../components/PanelState";

export function LaunchTargetPicker({
  choices,
  active,
  search,
  pending,
  error,
  retry,
  disabled,
  url,
  onSearch,
  onSelect,
  onPrepare,
  onUrl,
  onTrack,
}: {
  readonly choices: readonly LaunchTarget[];
  readonly active: LaunchTarget | undefined;
  readonly search: string;
  readonly pending: boolean;
  readonly error: string | undefined;
  readonly retry: () => void;
  readonly disabled: boolean;
  readonly url: string;
  readonly onSearch: (value: string) => void;
  readonly onSelect: (target: LaunchTarget) => void;
  readonly onPrepare?: () => void;
  readonly onUrl: (value: string) => void;
  readonly onTrack: () => void;
}) {
  const [searchOpen, setSearchOpen] = useState(false);
  const options =
    active && !choices.some((choice) => choice.reviewId === active.reviewId)
      ? [active, ...choices]
      : choices;
  return (
    <div className="launch-picker stack">
      <div className="launch-picker-heading">
        <label htmlFor="launch-review-target">Merge request</label>
        <button
          type="button"
          className="quiet-button"
          aria-expanded={searchOpen}
          disabled={disabled}
          onClick={() => {
            if (searchOpen && search) onSearch("");
            setSearchOpen(!searchOpen);
          }}
        >
          {searchOpen ? "Close search" : "Search"}
        </button>
      </div>
      {searchOpen && (
        <label className="field">
          <span className="sr-only">Search open merge requests</span>
          <input
            type="search"
            maxLength={200}
            value={search}
            disabled={disabled}
            placeholder="Search ticket or title"
            onChange={(event) => onSearch(event.currentTarget.value)}
          />
        </label>
      )}
      <select
        id="launch-review-target"
        disabled={disabled || !options.length}
        value={active?.reviewId ?? ""}
        onChange={(event) => {
          const chosen = choices.find((item) => item.reviewId === event.currentTarget.value);
          if (chosen) onSelect(chosen);
        }}
      >
        <option value="" disabled>
          Choose a merge request
        </option>
        {options.map((target) => (
          <option value={target.reviewId} key={target.reviewId}>
            {[
              ...target.tickets.map((ticket) => ticket.key),
              `${target.projectPath} !${target.iid}`,
            ].join(" · ")}
          </option>
        ))}
      </select>
      {pending && !choices.length && <Loading label="Finding merge requests…" />}
      {error && <Notice retry={retry}>{error}</Notice>}
      {search && !pending && !error && !choices.length && (
        <p className="meta">No matching open merge requests.</p>
      )}
      {active && <p className="picked-mr">{active.tickets[0]?.title ?? active.title}</p>}
      {onPrepare && (
        <div className="actions">
          <button
            type="button"
            className="primary"
            disabled={!active || disabled}
            onClick={onPrepare}
          >
            Prepare review
          </button>
        </div>
      )}
      <details className="launch-disclosure track-launch">
        <summary>Track MR by URL</summary>
        <form
          className="stack"
          onSubmit={(event) => {
            event.preventDefault();
            onTrack();
          }}
        >
          <label className="field">
            GitLab merge request
            <input
              type="url"
              disabled={disabled}
              value={url}
              maxLength={2048}
              onChange={(event) => onUrl(event.currentTarget.value)}
              placeholder="https://gitlab.example.com/team/project/-/merge_requests/42"
            />
          </label>
          <div className="actions">
            <button type="submit" disabled={disabled || !url.trim()}>
              Add to my tracked reviews
            </button>
          </div>
        </form>
      </details>
    </div>
  );
}
