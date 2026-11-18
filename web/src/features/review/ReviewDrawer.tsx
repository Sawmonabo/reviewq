import type { ReviewOverview, RunDetail } from "../../api/generated/contracts";

type ReviewDrawerProps = {
  readonly overview: ReviewOverview;
  readonly run: RunDetail | undefined;
  readonly onRun: (runId: string) => void;
  readonly onClose: () => void;
};

export function ReviewDrawer({ overview, run, onRun, onClose }: ReviewDrawerProps) {
  const { review } = overview;
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
    </aside>
  );
}
