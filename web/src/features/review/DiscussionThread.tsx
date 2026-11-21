import { useState } from "react";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import type { PatchDiscussion, PatchDiscussionNote } from "../../api/generated/contracts";
import { formatActivityTime, safeUrl } from "../../lib/presentation";

const CONFIGURED_REVIEWER = "Configured reviewer";

export function DiscussionThread({
  thread,
  outside = false,
  expanded,
  onExpanded,
  previewNote,
}: {
  readonly thread: PatchDiscussion;
  readonly outside?: boolean;
  readonly expanded?: boolean | undefined;
  readonly onExpanded?: (open: boolean) => void;
  readonly previewNote?: ((note: PatchDiscussionNote) => string | undefined) | undefined;
}) {
  const [localOpen, setLocalOpen] = useState(() => thread.resolution !== "resolved");
  const open = expanded ?? localOpen;
  const first = thread.notes[0];
  const source = safeUrl(thread.webUrl);
  const line = thread.anchor?.newLine ?? thread.anchor?.oldLine;
  const lineSide = thread.anchor?.newLine == null ? "old" : "new";
  const path = thread.anchor?.newLine == null ? thread.anchor?.oldPath : thread.anchor.newPath;
  const head = thread.anchor?.headSha;
  if (!first) return null;
  const status = { open: "Open", resolved: "Resolved", comment: "Comment", unknown: "Discussion" }[
    thread.resolution
  ];
  const subject =
    first.body
      .replace(/<!--[\s\S]*?-->|<[^>]+>/g, "")
      .split("\n")
      .map((line) => line.trim())
      .find(Boolean)
      ?.replace(/^#{1,6}\s+|\*\*|__|[_*`](?=\S)|(?<=\S)[_*`]/g, "")
      .slice(0, 150) || "Discussion";
  return (
    <details
      className="patch-discussion"
      data-discussion-id={thread.discussionId}
      open={open}
      onToggle={(event) => {
        const next = event.currentTarget.open;
        if (onExpanded) onExpanded(next);
        else setLocalOpen(next);
      }}
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
                  {line !== null && line !== undefined ? `:${lineSide} ${line}` : ""}
                </code>
              )}
              {thread.outdated && typeof head === "string" && (
                <span title={head}>Commented on {head.slice(0, 12)}</span>
              )}
            </div>
          )}
          {thread.notes.map((note) => {
            const preview = previewNote?.(note);
            if (preview !== undefined)
              return (
                <article
                  className="prose"
                  data-discussion-source={note.noteId}
                  key={note.noteId}
                  style={{ whiteSpace: "pre-wrap" }}
                >
                  <div className="meta">
                    {note.author}
                    {note.ours && ` · ${CONFIGURED_REVIEWER}`}
                    {note.createdAt && ` · ${formatActivityTime(note.createdAt)}`}
                  </div>
                  {preview}
                </article>
              );
            const href = safeUrl(note.webUrl);
            const stamp = note.createdAt ? (
              <time dateTime={note.createdAt}>{formatActivityTime(note.createdAt)}</time>
            ) : (
              "Open note in GitLab"
            );
            return (
              <article className="discussion-note stack" key={note.noteId}>
                <header className="discussion-note-meta">
                  <span className="avatar" aria-hidden="true">
                    {note.author.slice(0, 2).toUpperCase()}
                  </span>
                  <strong title={note.author}>{note.author}</strong>
                  {note.ours && <span className="meta">{CONFIGURED_REVIEWER}</span>}
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
