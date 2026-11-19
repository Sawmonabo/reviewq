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
  return (
    <div className="launch-picker stack">
      <label className="field">
        Search open merge requests
        <input
          type="search"
          maxLength={200}
          value={search}
          placeholder="Search ticket or title"
          onChange={(event) => onSearch(event.currentTarget.value)}
        />
      </label>
      <label className="field">
        Merge request
        <select
          value={active?.reviewId ?? ""}
          onChange={(event) => {
            const chosen = choices.find((item) => item.reviewId === event.currentTarget.value);
            if (chosen) onSelect(chosen);
          }}
        >
          <option value="" disabled>
            Choose a merge request
          </option>
          {active && !choices.some((item) => item.reviewId === active.reviewId) && (
            <option value={active.reviewId}>
              {active.projectPath}!{active.iid} · {active.title}
            </option>
          )}
          {choices.map((target) => (
            <option value={target.reviewId} key={target.reviewId}>
              {target.tickets[0]?.key ? `${target.tickets[0].key} · ` : ""}
              {target.projectPath}!{target.iid} · {target.title}
            </option>
          ))}
        </select>
      </label>
      {pending && !choices.length && <Loading label="Finding merge requests…" />}
      {error && <Notice retry={retry}>{error}</Notice>}
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
