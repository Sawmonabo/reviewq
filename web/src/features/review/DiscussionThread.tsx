import { useState } from "react";
import { Icon } from "../../components/Icon";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import type { PatchDiscussion, PatchDiscussionNote } from "../../api/generated/contracts";
import { formatActivityTime, safeUrl } from "../../lib/presentation";
import "./discussion-thread.css";

const CONFIGURED_REVIEWER = "Configured reviewer";

export function DiscussionThread({
  thread,
  outside = false,
  expanded,
  onExpanded,
  previewNote,
  presentation,
  onJump,
}: {
  readonly thread: PatchDiscussion;
  readonly outside?: boolean;
  readonly expanded?: boolean | undefined;
  readonly onExpanded?: (open: boolean) => void;
  readonly previewNote?: ((note: PatchDiscussionNote) => string | undefined) | undefined;
  readonly presentation?: "changes";
  readonly onJump?: (() => void) | undefined;
}) {
  const [localOpen, setLocalOpen] = useState(() => thread.resolution !== "resolved");
  const open = expanded ?? localOpen;
  const first = thread.notes[0];
  const source = safeUrl(thread.webUrl);
  const changes = presentation === "changes";
  const anchor = thread.anchor;
  const endpoint = changes ? (anchor?.range?.end ?? anchor) : anchor;
  const line = endpoint?.newLine ?? endpoint?.oldLine;
  const lineSide = endpoint?.newLine == null ? "old" : "new";
  const path = lineSide === "old" ? anchor?.oldPath : anchor?.newPath;
  const head = anchor?.headSha;
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
      data-presentation={presentation}
      data-resolution={thread.resolution}
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
            className={changes ? "discussion-status" : "row-tag"}
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
          {(changes || !open) && thread.outdated && <small>Earlier revision</small>}
          {!open && !thread.outdated && outside && <small>Outside the displayed diff</small>}
          {thread.notes.length > 1 && <small>{thread.notes.length} comments</small>}
        </span>
      </summary>
      {open && (
        <>
          {(path || thread.outdated || outside) && (
            <div className="discussion-location">
              {changes ? (
                <>
                  {path && <code>{path}</code>}
                  {endpoint?.oldLine != null && (
                    <span className="discussion-line">Old {endpoint.oldLine}</span>
                  )}
                  {endpoint?.newLine != null && (
                    <span className="discussion-line">New {endpoint.newLine}</span>
                  )}
                  {thread.outdated && typeof head === "string" && (
                    <span title={head}>
                      Source <code>{head.slice(0, 12)}</code>
                    </span>
                  )}
                  {!thread.outdated && outside && <span>Outside the displayed diff</span>}
                  {onJump && (
                    <button type="button" className="discussion-jump" onClick={onJump}>
                      Jump to line
                    </button>
                  )}
                </>
              ) : (
                <>
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
                      {line != null ? `:${lineSide} ${line}` : ""}
                    </code>
                  )}
                  {thread.outdated && typeof head === "string" && (
                    <span title={head}>Commented on {head.slice(0, 12)}</span>
                  )}
                </>
              )}
            </div>
          )}
          {thread.notes.map((note) => {
            const preview = previewNote?.(note);
            if (preview !== undefined && !changes)
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
              <article
                className="discussion-note stack"
                data-discussion-source={preview !== undefined ? note.noteId : undefined}
                key={note.noteId}
              >
                <header className="discussion-note-meta">
                  <span className="avatar" aria-hidden="true">
                    {changes ? authorInitials(note.author) : note.author.slice(0, 2).toUpperCase()}
                  </span>
                  <strong title={note.author}>{note.author}</strong>
                  {note.ours && (
                    <span className="discussion-note-reviewer meta">{CONFIGURED_REVIEWER}</span>
                  )}
                  {href ? (
                    <a
                      className={changes && !note.createdAt ? "discussion-note-link" : "meta"}
                      target="_blank"
                      rel="noreferrer"
                      href={href}
                      aria-label={changes && !note.createdAt ? "Open note in GitLab" : undefined}
                      title="Open note in GitLab"
                    >
                      {changes && !note.createdAt ? (
                        <Icon name="external" size={11} />
                      ) : (
                        <>{stamp} ↗</>
                      )}
                    </a>
                  ) : (
                    note.createdAt && <span className="meta">{stamp}</span>
                  )}
                </header>
                {preview === undefined ? (
                  <MarkdownContent source={note.body} base={source} />
                ) : (
                  <div className="discussion-note-preview">{preview}</div>
                )}
              </article>
            );
          })}
        </>
      )}
    </details>
  );
}

function authorInitials(author: string) {
  const words = author.trim().split(/\s+/);
  const first = Array.from(words[0]!);
  const initials = words.length > 1 ? [first[0], Array.from(words.at(-1)!)[0]] : first.slice(0, 2);
  return initials.join("").toUpperCase();
}
