import { lazy, Suspense, useState } from "react";
import { readTicket } from "../../api/tickets";
import type { TicketCommentOrder } from "../../api/tickets";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { formatTime, safeUrl } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import { jiraWiki } from "./wiki";
import "./tickets.css";

const Markdown = lazy(() =>
  import("../../components/markdown/MarkdownContent").then((module) => ({
    default: module.MarkdownContent,
  })),
);

export default function Tickets({ keys }: { readonly keys: readonly string[] }) {
  const bookmark = `ticket.${keys.join(".")}`;
  const [preferred, setPreferred] = useState(() => savedValue(bookmark));
  const current = typeof preferred === "string" && keys.includes(preferred) ? preferred : keys[0];
  if (!current) return <Empty title="No linked Jira ticket" />;
  return (
    <section
      className="ticket-pane"
      data-single={keys.length === 1}
      aria-label="Linked Jira ticket"
    >
      {keys.length > 1 && (
        <label className="field">
          Linked ticket
          <select
            value={current}
            onChange={(event) => {
              setPreferred(event.currentTarget.value);
              saveValue(bookmark, event.currentTarget.value);
            }}
          >
            {keys.map((key) => (
              <option key={key}>{key}</option>
            ))}
          </select>
        </label>
      )}
      <TicketDetail key={current} ticketKey={current} />
    </section>
  );
}

function TicketDetail({ ticketKey }: { readonly ticketKey: string }) {
  const [order, setOrder] = useState<TicketCommentOrder>("newest");
  const [trail, setTrail] = useState<readonly string[]>([]);
  const cursor = trail.at(-1);
  const detail = useResource(
    JSON.stringify(["ticket", ticketKey, order, cursor]),
    (signal) => readTicket(ticketKey, order, cursor, signal),
    [{ kind: "ticket", id: ticketKey }],
  );
  const ticket = detail.data;
  if (!ticket)
    return detail.error ? (
      <Notice retry={trail.length ? () => setTrail([]) : detail.retry}>{detail.error}</Notice>
    ) : (
      <Loading label={`Loading ${ticketKey}…`} />
    );
  const url = safeUrl(ticket.url);
  return (
    <>
      <header className="ticket-heading">
        <div className="ticket-status-line">
          {ticket.status && <span className="badge ticket-status green">{ticket.status}</span>}
          {ticket.issueType && <span>{ticket.issueType}</span>}
          {url && (
            <a className="ticket-jira-link" href={url} target="_blank" rel="noreferrer">
              Open in Jira ↗
            </a>
          )}
        </div>
        <h3 className="ticket-summary">{ticket.title}</h3>
        <dl className="ticket-facts">
          <div>
            <dt>Priority</dt>
            <dd>
              {ticket.priority && (
                <span
                  className="ticket-priority"
                  data-priority={ticket.priority.toLowerCase()}
                  aria-hidden="true"
                >
                  »
                </span>
              )}
              {ticket.priority ?? "Unknown"}
            </dd>
          </div>
          <div>
            <dt>Story Points</dt>
            <dd>
              <span
                className="ticket-avatar ticket-points"
                title={ticket.estimate === null ? "Unestimated" : `${ticket.estimate} story points`}
              >
                {ticket.estimate ?? "—"}
              </span>
            </dd>
          </div>
          <div>
            <dt>Assignee</dt>
            <dd>
              <TicketPerson name={ticket.assignee} fallback="Unassigned" />
            </dd>
          </div>
          <div>
            <dt>Reporter</dt>
            <dd>
              <TicketPerson name={ticket.reporter} fallback="Unknown" />
            </dd>
          </div>
        </dl>
        {ticket.labels.length > 0 && (
          <div className="actions">
            <span className="meta">Labels</span>
            {ticket.labels.map((label) => (
              <span className="badge" key={label}>
                {label}
              </span>
            ))}
          </div>
        )}
        {ticket.components.length > 0 && (
          <div className="actions">
            <span className="meta">Components</span>
            {ticket.components.map((component) => (
              <span className="badge" key={component}>
                {component}
              </span>
            ))}
          </div>
        )}
      </header>
      {detail.error && <Notice retry={detail.retry}>{detail.error}</Notice>}
      {ticket.warnings.map((warning) => (
        <Notice key={warning}>{warning}</Notice>
      ))}
      {ticket.description && (
        <section className="ticket-description" aria-label="Description and scope">
          <h3>Description &amp; scope</h3>
          <Suspense fallback={<pre className="prose-source">{ticket.description}</pre>}>
            <Markdown source={jiraWiki(ticket.description)} base={url} />
          </Suspense>
        </section>
      )}
      <section className="ticket-comments stack" aria-label="Jira comments">
        <div className="spread">
          <h3>
            Comments <span className="badge">{ticket.commentCount}</span>
          </h3>
          <label>
            Sort{" "}
            <select
              aria-label="Sort ticket comments"
              value={order}
              disabled={ticket.commentCount < 2}
              onChange={(event) => {
                setOrder(event.currentTarget.value === "oldest" ? "oldest" : "newest");
                setTrail([]);
              }}
            >
              <option value="newest">Newest first</option>
              <option value="oldest">Oldest first</option>
            </select>
          </label>
        </div>
        {!ticket.items.length && <p className="meta">No comments.</p>}
        {ticket.items.map((comment) => (
          <article className="surface-card ticket-comment" key={comment.id}>
            <div className="spread">
              <strong>{comment.author ?? "Unknown author"}</strong>
              <time className="meta" dateTime={comment.createdAt}>
                {formatTime(comment.createdAt)}
              </time>
            </div>
            {comment.body.length > 1800 ? (
              <details>
                <summary>Read comment</summary>
                <Suspense fallback={null}>
                  <Markdown source={jiraWiki(comment.body)} base={url} />
                </Suspense>
              </details>
            ) : (
              <Suspense fallback={<p>{comment.body}</p>}>
                <Markdown source={jiraWiki(comment.body)} base={url} />
              </Suspense>
            )}
            {safeUrl(comment.url) && (
              <a className="meta" href={safeUrl(comment.url)} target="_blank" rel="noreferrer">
                Comment permalink ↗
              </a>
            )}
          </article>
        ))}
        <div className="actions">
          {trail.length > 0 && (
            <button type="button" onClick={() => setTrail(trail.slice(0, -1))}>
              Previous comments
            </button>
          )}
          {ticket.nextCursor && (
            <button
              type="button"
              onClick={() => {
                if (ticket.nextCursor) setTrail([...trail, ticket.nextCursor]);
              }}
            >
              More comments
            </button>
          )}
        </div>
      </section>
    </>
  );
}

function TicketPerson({
  name,
  fallback,
}: {
  readonly name: string | null;
  readonly fallback: string;
}) {
  if (!name) return <span className="meta">{fallback}</span>;
  const initials = name
    .trim()
    .split(/\s+/)
    .slice(0, 2)
    .map((part) => part.charAt(0))
    .join("")
    .toUpperCase();
  return (
    <>
      <span className="ticket-avatar" aria-hidden="true">
        {initials}
      </span>
      <span>{name}</span>
    </>
  );
}
