import { memo, useMemo, useState } from "react";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import type { TicketComment } from "../../api/generated/contracts";
import { Loading, Notice } from "../../components/PanelState";
import { formatActivityTime, safeUrl } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import { useTicket } from "./useTicket";
import { jiraWiki } from "./wiki";
import "./tickets.css";

export default function Tickets({ keys }: { readonly keys: readonly string[] }) {
  const bookmark = `ticket.${keys.join(".")}`;
  const [preferred, setPreferred] = useState(() => savedValue(bookmark));
  const current = typeof preferred === "string" && keys.includes(preferred) ? preferred : keys[0];
  if (!current) return <p className="meta">No ticket is linked to this merge request.</p>;
  return (
    <section
      className="ticket-pane"
      data-single={keys.length === 1}
      aria-label="Linked Jira ticket"
    >
      {keys.length > 1 && (
        <label className="field">
          Ticket
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
  const { ticket, order, trail, switching, error, unavailable, select, retry } =
    useTicket(ticketKey);
  if (!ticket)
    return error ? (
      <Notice retry={retry}>{error}</Notice>
    ) : unavailable ? (
      <p className="meta">Ticket information is unavailable.</p>
    ) : (
      <Loading label={`Loading ${ticketKey}…`} />
    );
  const url = safeUrl(ticket.url);
  return (
    <>
      <header className="ticket-heading">
        <div className="ticket-status-line">
          {ticket.status && <span className="badge ticket-status green">{ticket.status}</span>}
          <span>{ticket.issueType || "Ticket"}</span>
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
              {ticket.priority ?? "Not specified"}
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
              <TicketPerson name={ticket.reporter} fallback="Not specified" />
            </dd>
          </div>
          {ticket.components.length > 0 && (
            <div>
              <dt>Components</dt>
              <dd>{ticket.components.join(", ")}</dd>
            </div>
          )}
          {ticket.labels.length > 0 && (
            <div>
              <dt>Labels</dt>
              <dd>{ticket.labels.join(", ")}</dd>
            </div>
          )}
        </dl>
      </header>
      {ticket.warnings.map((warning) => (
        <Notice key={warning}>{warning}</Notice>
      ))}
      <section className="ticket-description" aria-label="Description and scope">
        <h3>Description &amp; scope</h3>
        {ticket.description?.trim() ? (
          <TicketMarkdown body={ticket.description} base={url} />
        ) : (
          <p className="meta">This ticket has no description.</p>
        )}
      </section>
      <section className="ticket-comments stack" aria-label="Jira comments">
        <header className="ticket-comments-heading spread">
          <h3>
            Comments <span className="badge">{ticket.commentCount}</span>
          </h3>
          <label>
            Sort{" "}
            <select
              aria-label="Sort ticket comments"
              value={order}
              onChange={(event) =>
                select(event.currentTarget.value === "oldest" ? "oldest" : "newest", [])
              }
            >
              <option value="newest">Newest first</option>
              <option value="oldest">Oldest first</option>
            </select>
          </label>
        </header>
        {error && <Notice retry={retry}>{error}</Notice>}
        {switching && !error && (
          <p role="status" className="sr-only">
            Loading comments.
          </p>
        )}
        {!ticket.items.length && <p className="meta">No comments yet.</p>}
        <div className="ticket-comment-list" aria-busy={switching && !error}>
          {ticket.items.map((comment) => (
            <CommentCard key={comment.id} comment={comment} base={url} />
          ))}
        </div>
        {(trail.length > 0 || ticket.nextCursor) && (
          <div className="actions">
            <button
              type="button"
              aria-disabled={switching || trail.length === 0}
              onClick={() => {
                if (!switching && trail.length) select(order, trail.slice(0, -1));
              }}
            >
              Previous comments
            </button>
            <button
              type="button"
              aria-disabled={switching || !ticket.nextCursor}
              onClick={() => {
                if (!switching && ticket.nextCursor) select(order, [...trail, ticket.nextCursor]);
              }}
            >
              More comments
            </button>
          </div>
        )}
      </section>
      <p className="ticket-freshness meta">
        {ticket.updatedAt && (
          <>
            Updated <time dateTime={ticket.updatedAt}>{formatActivityTime(ticket.updatedAt)}</time>{" "}
            ·{" "}
          </>
        )}
        Loaded <time dateTime={ticket.fetchedAt}>{formatActivityTime(ticket.fetchedAt)}</time>
      </p>
    </>
  );
}

const TicketMarkdown = memo(function TicketMarkdown({
  body,
  base,
}: {
  readonly body: string;
  readonly base?: string | undefined;
}) {
  const markdown = useMemo(() => jiraWiki(body), [body]);
  return <MarkdownContent source={markdown} base={base} />;
});

function CommentCard({
  comment,
  base,
}: {
  readonly comment: TicketComment;
  readonly base?: string | undefined;
}) {
  const [expanded, setExpanded] = useState(false);
  const collapsible = comment.body.length > 1400;
  const permalink = safeUrl(comment.url);
  const stamp = <time dateTime={comment.createdAt}>{formatActivityTime(comment.createdAt)}</time>;
  const folded = collapsible && !expanded;
  return (
    <article className="ticket-comment">
      <div className="ticket-comment-meta">
        <strong>
          <TicketPerson name={comment.author} fallback="Unknown author" />
        </strong>
        {permalink ? (
          <a href={permalink} target="_blank" rel="noreferrer" title="Open comment in Jira">
            {stamp}
          </a>
        ) : (
          stamp
        )}
      </div>
      <div
        className={folded ? "ticket-comment-body ticket-comment-folded" : "ticket-comment-body"}
        onFocusCapture={(event) => {
          const clip = event.currentTarget.getBoundingClientRect();
          if (folded && event.target.getBoundingClientRect().bottom > clip.top + clip.height * 0.73)
            setExpanded(true);
        }}
      >
        <TicketMarkdown body={comment.body} base={base} />
      </div>
      {collapsible && (
        <button
          type="button"
          className="ticket-comment-toggle"
          aria-expanded={expanded}
          onClick={() => setExpanded(!expanded)}
        >
          {expanded ? "Collapse comment" : "Read full comment"}
        </button>
      )}
    </article>
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
