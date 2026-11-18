import type { KeyboardEvent } from "react";
import type { ReviewOverview, RunDetail } from "../../api/generated/contracts";
import { Changes } from "./changes/Changes";

type ReviewTab = "overview" | "changes";
const TABS: readonly { readonly id: ReviewTab; readonly label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "changes", label: "Changes" },
];

type ReviewDrawerProps = {
  readonly overview: ReviewOverview;
  readonly run: RunDetail | undefined;
  readonly tab: ReviewTab;
  readonly onTab: (tab: ReviewTab) => void;
  readonly onRun: (runId: string) => void;
  readonly onClose: () => void;
};

export function ReviewDrawer({ overview, run, tab, onTab, onRun, onClose }: ReviewDrawerProps) {
  const { review } = overview;
  function moveTab(event: KeyboardEvent<HTMLDivElement>) {
    const current = TABS.findIndex((item) => item.id === tab);
    const position =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? TABS.length - 1
          : event.key === "ArrowRight"
            ? (current + 1) % TABS.length
            : event.key === "ArrowLeft"
              ? (current + TABS.length - 1) % TABS.length
              : undefined;
    if (position === undefined) return;
    const item = TABS[position];
    if (!item) return;
    event.preventDefault();
    onTab(item.id);
    event.currentTarget.querySelectorAll("button")[position]?.focus();
  }
  return (
    <aside className="review-drawer" aria-labelledby="review-title">
      <div className="drawer-heading">
        <div>
          <span className="review-project">
            {review.projectPath}!{review.iid}
          </span>
          <h2 id="review-title">{review.title}</h2>
        </div>
        <button onClick={onClose} aria-label="Close review">
          Close
        </button>
      </div>
      {overview.runs.items.length > 0 && (
        <label>
          Run{" "}
          <select value={run?.runId ?? ""} onChange={(event) => onRun(event.target.value)}>
            {overview.runs.items.map((item) => (
              <option key={item.runId} value={item.runId}>
                {item.provider} · {item.startedAt ?? item.runId}
              </option>
            ))}
          </select>
        </label>
      )}
      <div role="tablist" aria-label="Review sections" className="review-tabs" onKeyDown={moveTab}>
        {TABS.map(({ id, label }) => (
          <button
            key={id}
            id={`review-tab-${id}`}
            role="tab"
            aria-selected={tab === id}
            aria-controls={`review-panel-${id}`}
            tabIndex={tab === id ? 0 : -1}
            onClick={() => onTab(id)}
          >
            {label}
          </button>
        ))}
      </div>
      <div role="tabpanel" id={`review-panel-${tab}`} aria-labelledby={`review-tab-${tab}`}>
        {tab === "changes" ? (
          <Changes key={review.reviewId} reviewId={review.reviewId} />
        ) : (
          <section className="review-overview" aria-label="Overview">
            <dl>
              <dt>State</dt>
              <dd>{review.mrState}</dd>
              <dt>Source revision</dt>
              <dd>
                <code>{review.sourceSha}</code>
              </dd>
              {run && (
                <>
                  <dt>Lead</dt>
                  <dd>
                    <code>{run.leadSessionKey}</code>
                  </dd>
                </>
              )}
            </dl>
          </section>
        )}
      </div>
    </aside>
  );
}
