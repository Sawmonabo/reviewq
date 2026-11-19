import { lazy, Suspense, useState } from "react";
import type { PatchDiscussion } from "../../api/generated/contracts";
import { formatTime, safeUrl } from "../../lib/presentation";
import { discussionStatus } from "./discussion-status";

const Markdown = lazy(() =>
  import("../../components/markdown/MarkdownContent").then((module) => ({
    default: module.MarkdownContent,
  })),
);

export function DiscussionThread({
  thread,
  outside = false,
}: {
  readonly thread: PatchDiscussion;
  readonly outside?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const first = thread.notes[0];
  const source = safeUrl(thread.webUrl);
  const line = thread.anchor?.newLine ?? thread.anchor?.oldLine;
  if (!first) return null;
  const status = { open: "Open", resolved: "Resolved", comment: "Comment", unknown: "Discussion" }[
    discussionStatus(thread)
  ];
  return (
    <details className="patch-discussion" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>
        <strong>{first.author}</strong> · {thread.outdated ? "Earlier revision" : status}
        {outside && " · Outside the displayed diff"}
        {thread.notes.length > 1 && ` · ${thread.notes.length} notes`}
      </summary>
      {source && (
        <a target="_blank" rel="noreferrer" href={source}>
          {thread.anchor ? `${thread.anchor.newPath}${line ? `:${line}` : ""}` : "Open in GitLab"} ↗
        </a>
      )}
      {open && (
        <Suspense fallback={<p>{first.body}</p>}>
          {thread.notes.map((note) => (
            <article className="discussion-note stack" key={note.noteId}>
              <div className="spread">
                <strong>
                  {note.author}
                  {note.ours ? " · You or a configured reviewer" : ""}
                </strong>
                {note.createdAt && (
                  <time className="meta" dateTime={note.createdAt}>
                    {formatTime(note.createdAt)}
                  </time>
                )}
              </div>
              <Markdown source={note.body} base={source} />
            </article>
          ))}
        </Suspense>
      )}
    </details>
  );
}
