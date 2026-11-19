import { useState } from "react";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import type { PatchDiscussion } from "../../api/generated/contracts";
import { formatTime, safeUrl } from "../../lib/presentation";

export function DiscussionThread({
  thread,
  outside = false,
}: {
  readonly thread: PatchDiscussion;
  readonly outside?: boolean;
}) {
  const [open, setOpen] = useState(() => thread.resolution !== "resolved");
  const first = thread.notes[0];
  const source = safeUrl(thread.webUrl);
  const line = thread.anchor?.newLine ?? thread.anchor?.oldLine;
  const path = thread.anchor?.newLine == null ? thread.anchor?.oldPath : thread.anchor.newPath;
  if (!first) return null;
  const status = { open: "Open", resolved: "Resolved", comment: "Comment", unknown: "Discussion" }[
    thread.resolution
  ];
  const subject =
    thread.notes
      .find((note) => note.body.trim())
      ?.body.trim()
      .split("\n", 1)[0]
      ?.trim()
      .match(/^.{1,140}/u)?.[0] ?? "Discussion";
  return (
    <details
      className="patch-discussion"
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>
        <span className="discussion-heading">
          <span
            className="row-tag"
            data-review-kind={
              thread.resolution === "open"
                ? "thread_reply"
                : thread.resolution === "resolved"
                  ? "reviewed"
                  : undefined
            }
          >
            {status}
          </span>
          <strong className="discussion-subject">{subject}</strong>
          {!open && thread.outdated && <small>Earlier revision</small>}
          {!open && !thread.outdated && outside && <small>Outside the displayed diff</small>}
          {thread.notes.length > 1 && <small>{thread.notes.length} comments</small>}
        </span>
      </summary>
      {open && (
        <>
          {(path || thread.outdated || outside) && (
            <div className="discussion-location">
              <strong>
                {thread.outdated
                  ? "Earlier revision"
                  : outside
                    ? "Outside the displayed diff"
                    : "Code discussion"}
              </strong>
              {path && (
                <code>
                  {path}
                  {line !== null && line !== undefined ? `:${line}` : ""}
                </code>
              )}
            </div>
          )}
          {thread.notes.map((note) => {
            const href = safeUrl(note.webUrl);
            const stamp = note.createdAt ? (
              <time dateTime={note.createdAt}>{formatTime(note.createdAt)}</time>
            ) : (
              "Open note in GitLab"
            );
            return (
              <article className="discussion-note stack" key={note.noteId}>
                <header className="discussion-note-meta">
                  <span className="avatar" aria-hidden="true">
                    {note.author.slice(0, 2).toUpperCase()}
                  </span>
                  <strong>{note.author}</strong>
                  {note.ours && <span className="meta">Configured reviewer</span>}
                  {href ? (
                    <a
                      className="meta"
                      target="_blank"
                      rel="noreferrer"
                      href={href}
                      title="Open note in GitLab"
                    >
                      {stamp} ↗
                    </a>
                  ) : (
                    note.createdAt && <span className="meta">{stamp}</span>
                  )}
                </header>
                <MarkdownContent source={note.body} base={source} />
              </article>
            );
          })}
        </>
      )}
    </details>
  );
}
