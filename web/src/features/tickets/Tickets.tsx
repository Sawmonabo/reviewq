import { lazy, Suspense, useState } from "react";
import { readTicket } from "../../api/tickets";
import { Empty, Loading, Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";
import { formatTime, safeUrl } from "../../lib/presentation";
import { savedValue, saveValue } from "../../lib/view-preferences";
import { jiraWiki } from "./wiki";
import "./tickets.css";

const Markdown = lazy(() => import("../../components/markdown/MarkdownContent").then((module) => ({ default: module.MarkdownContent })));

export default function Tickets({ keys }: { readonly keys: readonly string[] }) {
  const bookmark = `ticket.${keys.join(".")}`;
  const [preferred, setPreferred] = useState(() => savedValue(bookmark));
  const current = typeof preferred === "string" && keys.includes(preferred) ? preferred : keys[0];
  if (!current) return <Empty title="No linked Jira ticket" />;
  return <section className="ticket-pane stack" aria-label="Linked Jira ticket">
    {keys.length > 1 && <label className="field">Linked ticket<select value={current} onChange={(event) => {
      setPreferred(event.currentTarget.value); saveValue(bookmark, event.currentTarget.value);
    }}>{keys.map((key) => <option key={key}>{key}</option>)}</select></label>}
    <TicketDetail key={current} ticketKey={current} />
  </section>;
}

function TicketDetail({ ticketKey }: { readonly ticketKey: string }) {
  const [trail, setTrail] = useState<readonly string[]>([]);
  const primary = useResource(`ticket:${ticketKey}`, (signal) => readTicket(ticketKey, undefined, signal), [{ kind: "ticket", id: ticketKey }]);
  const comments = useResource(`ticket-comments:${ticketKey}:${trail.at(-1) ?? ""}`, (signal) => {
    const cursor = trail.at(-1);
    return cursor ? readTicket(ticketKey, cursor, signal) : Promise.resolve({ data: null, cursor: "" });
  }, [{ kind: "ticket", id: ticketKey }]);
  const ticket = primary.data;
  if (!ticket) return primary.error ? <Notice retry={primary.retry}>{primary.error}</Notice> : <Loading label={`Loading ${ticketKey}…`} />;
  const url = safeUrl(ticket.url);
  const page = trail.length ? comments.data : ticket;
  return <>
    <header className="stack">
      <div className="actions"><span className="ticket-key">{ticket.key}</span>{ticket.status && <span className="badge blue">{ticket.status}</span>}{ticket.issueType && <span className="badge">{ticket.issueType}</span>}
        {url && <a href={url} target="_blank" rel="noreferrer">Open in Jira ↗</a>}</div>
      <h3>{ticket.title}</h3>
      <dl className="detail-facts">
        <div><dt>Priority</dt><dd>{ticket.priority ?? "Unknown"}</dd></div>
        <div><dt>Story Points</dt><dd>{ticket.estimate === null ? "Unestimated" : ticket.estimate}</dd></div>
        <div><dt>Assignee</dt><dd>{ticket.assignee ?? "Unassigned"}</dd></div>
        <div><dt>Reporter</dt><dd>{ticket.reporter ?? "Unknown"}</dd></div>
      </dl>
      {ticket.labels.length > 0 && <div className="actions"><span className="meta">Labels</span>{ticket.labels.map((label) => <span className="badge" key={label}>{label}</span>)}</div>}
      {ticket.components.length > 0 && <div className="actions"><span className="meta">Components</span>{ticket.components.map((component) => <span className="badge" key={component}>{component}</span>)}</div>}
    </header>
    {primary.error && <Notice retry={primary.retry}>{primary.error}</Notice>}
    {ticket.warnings.map((warning) => <Notice key={warning}>{warning}</Notice>)}
    {ticket.description && <section className="surface-card stack" aria-label="Description"><h3>Description</h3>
      <Suspense fallback={<pre className="prose-source">{ticket.description}</pre>}><Markdown source={jiraWiki(ticket.description)} base={url} /></Suspense>
    </section>}
    <section className="stack" aria-label="Jira comments">
      <h3>Comments</h3>
      {comments.error && <Notice retry={comments.retry}>{comments.error}</Notice>}
      {trail.length > 0 && comments.pending && <Loading label="Loading Jira comments…" />}
      {!page?.items.length && !trail.length && <p className="meta">No comments.</p>}
      {page?.items.map((comment) => <article className="surface-card ticket-comment" key={comment.id}>
        <div className="spread"><strong>{comment.author ?? "Unknown author"}</strong>
          <time className="meta" dateTime={comment.createdAt}>{formatTime(comment.createdAt)}</time></div>
        {comment.body.length > 1800 ? <details><summary>Read comment</summary><Suspense fallback={null}><Markdown source={jiraWiki(comment.body)} base={url} /></Suspense></details>
          : <Suspense fallback={<p>{comment.body}</p>}><Markdown source={jiraWiki(comment.body)} base={url} /></Suspense>}
        {url && <a className="meta" href={`${url}#comment-${encodeURIComponent(comment.id)}`} target="_blank" rel="noreferrer">Comment permalink ↗</a>}
      </article>)}
      <div className="actions">
        {trail.length > 0 && <button type="button" onClick={() => setTrail(trail.slice(0, -1))}>Previous comments</button>}
        {page?.nextCursor && <button type="button" onClick={() => { if (page.nextCursor) setTrail([...trail, page.nextCursor]); }}>More comments</button>}
      </div>
    </section>
  </>;
}
