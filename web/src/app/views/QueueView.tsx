import type { QueuePage } from "../../api/generated/contracts";

type QueueProps = {
  readonly queue: QueuePage | undefined;
  readonly onReview: (reviewId: string) => void;
};

export function QueueView({ queue, onReview }: QueueProps) {
  if (!queue) return null;
  return (
    <>
      {queue.sync.error && (
        <p className="workspace-error" role="status">
          {queue.sync.error.message}
          {queue.sync.lastSuccessAt &&
            ` Showing reviews from ${new Date(queue.sync.lastSuccessAt).toLocaleString()}.`}
        </p>
      )}
      {queue.items.length === 0 ? (
        <p className="empty-queue">No reviews are in the queue.</p>
      ) : (
        <ul className="review-list">
          {queue.items.map((item) =>
            item.kind === "mr" ? (
              <li key={item.reviewId}>
                <button className="review-row" onClick={() => onReview(item.reviewId)}>
                  <span className="review-project">
                    {item.projectPath}!{item.iid}
                  </span>
                  <strong>{item.title}</strong>
                  <span className="review-run-count">
                    {item.registeredLeadCount === 0
                      ? "No runs"
                      : `${item.registeredLeadCount} ${item.registeredLeadCount === 1 ? "run" : "runs"}`}
                  </span>
                </button>
              </li>
            ) : (
              <li key={item.key} className="review-row">
                <span className="review-project">{item.key}</span>
                <strong>{item.title}</strong>
              </li>
            ),
          )}
        </ul>
      )}
    </>
  );
}
