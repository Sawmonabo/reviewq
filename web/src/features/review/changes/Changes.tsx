import { useRef, useState } from "react";
import type { ReactNode } from "react";
import { ResponseSizeLimit } from "../../../api/bounded-bytes";
import { ApiResponseError } from "../../../api/client";
import {
  downloadPatchUrl,
  isRichPatch,
  PATCH_WINDOW_BYTES,
  PatchFormattingLimit,
  readPatch,
  readPatchDiscussions,
  readPatchFiles,
} from "../../../api/changes";
import type { PatchDiscussion, PatchFile, PatchFileIndex } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Pagination } from "../../../components/Pagination";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { safeUrl } from "../../../lib/presentation";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { DiscussionThread } from "../DiscussionThread";
import { Approvals } from "./Approvals";
import { PatchDocumentView } from "./PatchDocumentView";
import "./changes.css";

type Boundary = "first" | "last";
type Position = {
  readonly revision: string;
  readonly cursor: string;
  readonly boundary?: Boundary;
};
type Tab = "files" | "general" | "all";
type Status = "any" | "open" | "resolved";
type Window = { readonly offset: number; readonly etag?: string; readonly last?: boolean };
const MAX_EMPTY_DISCUSSION_PAGES = 4;
const tabs = [
  ["files", "Files"],
  ["general", "General comments"],
  ["all", "All conversations"],
] as const;

export default function Changes({
  reviewId,
  webUrl,
  children,
}: {
  readonly reviewId: string;
  readonly webUrl: string;
  readonly children?: ReactNode;
}) {
  const [fileId, setFileId] = useState(() => storedFile(reviewId));
  const [position, setPosition] = useState<Position>();
  const [epoch, setEpoch] = useState(0);
  const [retained, setRetained] = useState<PatchFileIndex>();
  const [tab, setTab] = useState<Tab>("files");
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<Status>("any");
  const [involving, setInvolving] = useState(false);
  const participant = useRef<HTMLSelectElement>(null);
  const indexKey = JSON.stringify([reviewId, epoch, position]);
  const index = useResource(indexKey, async (signal) => {
    try {
      const result = await readPatchFiles(
        reviewId,
        signal,
        position,
        position || epoch > 0 ? undefined : storedFile(reviewId),
      );
      if (position?.boundary && !signal.aborted) {
        const selected =
          position.boundary === "last" ? result.data.items.at(-1) : result.data.items[0];
        if (selected) selectFile(selected);
      }
      return result;
    } catch (error) {
      if (
        signal.aborted ||
        position ||
        !storedFile(reviewId) ||
        !(error instanceof ApiResponseError)
      )
        throw error;
      return readPatchFiles(reviewId, signal);
    }
  });
  const shown = index.data ?? retained;
  const revision = shown?.revision;
  const current = useResource(
    `change-current:${reviewId}`,
    (signal) => readPatchFiles(reviewId, signal),
    [{ kind: "changes", id: reviewId }],
  );
  const changed = Boolean(revision && current.data && current.data.revision !== revision);
  const [threadTrail, setThreadTrail] = useState<readonly string[]>([]);
  const discussionScope = tab === "general" ? "general" : "all";
  const filtering = involving || status !== "any";
  const threads = useResource(
    JSON.stringify([
      "discussions",
      reviewId,
      revision,
      discussionScope,
      involving,
      status,
      threadTrail.at(-1),
    ]),
    async (signal) => {
      if (!revision) return { data: null, cursor: "" };
      let page = await readPatchDiscussions(reviewId, revision, signal, threadTrail.at(-1));
      if (!filtering && discussionScope === "all") return page;
      for (let skipped = 0; skipped < MAX_EMPTY_DISCUSSION_PAGES; skipped++) {
        const next = page.data.nextCursor;
        if (
          !next ||
          signal.aborted ||
          page.data.items.some(
            (thread) =>
              matchesFilters(thread, status, involving) &&
              (discussionScope === "all" || isGeneral(thread)),
          )
        )
          break;
        page = await readPatchDiscussions(reviewId, revision, signal, next);
      }
      return page;
    },
    [{ kind: "discussions", id: reviewId }],
  );
  const available =
    shown?.items.filter((file) =>
      `${file.newPath} ${file.oldPath}`.toLowerCase().includes(query.toLowerCase()),
    ) ?? [];
  const files = shown?.items ?? [];
  const file = files.find((entry) => entry.fileId === fileId) ?? files[0];
  const filePosition = file ? files.indexOf(file) : -1;
  const pageNumber = shown?.pageNumber ?? 1;
  const fileSearch = useRef<HTMLInputElement>(null);
  const picker = useRef<HTMLDetailsElement>(null);
  const filtered = (threads.data?.items ?? []).filter((thread) =>
    matchesFilters(thread, status, involving),
  );
  const general = filtered.filter(isGeneral);
  const anchors = file ? filtered.filter((thread) => matches(thread, file)) : [];
  const visible =
    tab === "files"
      ? filtered.filter((thread) => !file || !matches(thread, file))
      : tab === "general"
        ? general
        : filtered;

  function selectFile(next: PatchFile) {
    setFileId(next.fileId);
    saveValue(`changes.file.${reviewId}`, next.fileId);
  }

  function refreshIndex() {
    setPosition(undefined);
    setRetained(undefined);
    setThreadTrail([]);
    setEpoch((value) => value + 1);
  }

  function movePage(cursor: string | null | undefined, boundary?: Boundary) {
    if (!cursor || !index.data || index.pending) return;
    setRetained(index.data);
    setPosition({ revision: index.data.revision, cursor, ...(boundary ? { boundary } : {}) });
  }

  function clearConversationFilters() {
    setInvolving(false);
    setStatus("any");
    setThreadTrail([]);
    participant.current?.focus({ preventScroll: true });
  }

  if (index.pending && !shown && !index.error) return <Loading label="Loading changes…" />;

  return (
    <section className="changes stack" aria-label="Changes">
      <div className="changes-top">
        <div>
          <h3>Changes &amp; conversations</h3>
          {shown && (
            <p className="meta">
              {files.length}
              {shown.previousCursor || shown.nextCursor ? "+" : ""} files · {filtered.length}
              {threads.data?.nextCursor ? "+" : ""} conversations
            </p>
          )}
        </div>
        <Approvals reviewId={reviewId} displayedSha={shown?.headSha} webUrl={webUrl} />
      </div>
      {children}
      {shown && (
        <div className="diff-revision">
          <span>
            Source <code>{shown.headSha.slice(0, 12)}</code>
          </span>
          <span>
            Base <code>{shown.baseSha.slice(0, 12)}</code>
          </span>
        </div>
      )}
      {changed && (
        <Notice>
          The source revision changed. The displayed patch remains on {shown?.headSha.slice(0, 12)}.
          <button type="button" onClick={refreshIndex}>
            Load current changes
          </button>
        </Notice>
      )}
      <div className="actions changes-tabs">
        {tabs.map(([id, label]) => (
          <button
            key={id}
            type="button"
            aria-pressed={tab === id}
            onClick={() => {
              if (tab !== id) {
                setTab(id);
                setThreadTrail([]);
              }
            }}
          >
            {label}{" "}
            <strong className="changes-count">
              {id === "files" ? files.length : id === "general" ? general.length : filtered.length}
              {(id === "files"
                ? shown?.previousCursor || shown?.nextCursor
                : threads.data?.nextCursor) && "+"}
            </strong>
          </button>
        ))}
      </div>
      <div className="actions thread-filters">
        <label>
          Show{" "}
          <select
            ref={participant}
            aria-label="Conversation participants"
            value={involving ? "mine" : "all"}
            onChange={(event) => {
              setInvolving(event.currentTarget.value === "mine");
              setThreadTrail([]);
            }}
          >
            <option value="all">All participants</option>
            <option value="mine">Involving you</option>
          </select>
        </label>
        <label>
          {" "}
          Status{" "}
          <select
            aria-label="Conversation status"
            value={status}
            onChange={(event) => {
              setStatus(event.currentTarget.value as Status);
              setThreadTrail([]);
            }}
          >
            <option value="any">Any status</option>
            <option value="open">Open only</option>
            <option value="resolved">Resolved only</option>
          </select>
        </label>
        {filtering && (
          <button
            type="button"
            className="filter-control clear-filters"
            aria-label="Clear conversation filters"
            title="Clear conversation filters"
            onClick={clearConversationFilters}
          >
            <Icon name="clearFilters" size={16} />
          </button>
        )}
      </div>
      {index.error && (
        <Notice retry={index.retry}>
          {index.error}{" "}
          {position && (
            <button type="button" onClick={refreshIndex}>
              Restart file list
            </button>
          )}
        </Notice>
      )}
      {tab === "files" && (
        <>
          {shown && (
            <>
              <details
                className="changed-file-picker"
                ref={picker}
                onToggle={(event) => {
                  if (event.currentTarget.open) fileSearch.current?.focus();
                }}
              >
                <summary>
                  Changed files{" "}
                  <small>
                    {files.length}
                    {shown.previousCursor || shown.nextCursor ? "+" : ""}
                  </small>
                </summary>
                <label className="search-field">
                  <Icon name="search" size={13} />
                  <input
                    ref={fileSearch}
                    aria-label="Search files on this page"
                    value={query}
                    placeholder="Find a file"
                    onChange={(event) => setQuery(event.currentTarget.value)}
                  />
                </label>
                <nav aria-label="Changed files" aria-busy={index.pending}>
                  {available.map((item) => (
                    <button
                      type="button"
                      key={item.fileId}
                      aria-current={item.fileId === file?.fileId || undefined}
                      onClick={() => selectFile(item)}
                    >
                      <span className="file-initial" aria-label={fileKind(item)}>
                        {fileKind(item).charAt(0)}
                      </span>
                      <code>
                        {item.renamed ? `${item.oldPath} → ${item.newPath}` : item.newPath}
                      </code>
                      <DiffCount file={item} />
                    </button>
                  ))}
                  {!available.length && (
                    <p className="meta">
                      {query ? "No matching files on this page." : "No changed files."}
                    </p>
                  )}
                </nav>
                {(shown.previousCursor || shown.nextCursor) && (
                  <nav className="pagination" aria-label="Changed file pages">
                    <button
                      type="button"
                      aria-disabled={!shown.previousCursor || index.pending}
                      onClick={() => movePage(shown.previousCursor)}
                    >
                      Previous
                    </button>
                    <span className="meta" role="status">
                      {index.pending ? "Loading files…" : `Page ${pageNumber}`}
                    </span>
                    <button
                      type="button"
                      aria-disabled={!shown.nextCursor || index.pending}
                      onClick={() => movePage(shown.nextCursor)}
                    >
                      Next
                    </button>
                  </nav>
                )}
              </details>
              {file && (
                <SelectedPatch
                  file={file}
                  threads={anchors}
                  webUrl={webUrl}
                  onReload={refreshIndex}
                  position={filePosition}
                  total={files.length}
                  previous={Boolean(shown.previousCursor)}
                  next={Boolean(shown.nextCursor)}
                  pending={index.pending}
                  onPrevious={() => {
                    if (index.pending) return;
                    const next = files[filePosition - 1];
                    if (next) selectFile(next);
                    else movePage(shown.previousCursor, "last");
                  }}
                  onNext={() => {
                    if (index.pending) return;
                    const next = files[filePosition + 1];
                    if (next) selectFile(next);
                    else movePage(shown.nextCursor, "first");
                  }}
                />
              )}
            </>
          )}
        </>
      )}
      {threads.error && <Notice retry={threads.retry}>{threads.error}</Notice>}
      {threads.pending && <Loading label="Loading discussions…" />}
      <section className="discussion-list stack" aria-label="Review discussions">
        <h3>
          {tab === "files"
            ? "Other discussions"
            : tab === "general"
              ? "General comments"
              : "All conversations"}
        </h3>
        {threads.data && (
          <p className="meta">
            {visible.length} {visible.length === 1 ? "discussion" : "discussions"} on this page
            {tab === "files" && anchors.length > 0 ? ` · ${anchors.length} on this file` : ""}
          </p>
        )}
        {threads.data && !visible.length && (
          <div className="changes-conversation-empty">
            <div role="status">
              <Icon name="inbox" size={28} />
              <h3>
                {threads.data.nextCursor
                  ? "No matching conversations on this page"
                  : filtering
                    ? tab === "general"
                      ? "No matching comments"
                      : "No matching conversations"
                    : tab === "general"
                      ? "No general comments yet"
                      : "No conversations yet"}
              </h3>
              {threads.data.nextCursor ? (
                <p className="meta">More discussion pages are available below.</p>
              ) : (
                filtering && <p className="meta">Try changing your filters.</p>
              )}
            </div>
            {filtering && (
              <button type="button" onClick={clearConversationFilters}>
                Clear filters
              </button>
            )}
          </div>
        )}
        {visible.map((thread) => (
          <DiscussionThread
            key={thread.discussionId}
            thread={thread}
            outside={Boolean(
              tab === "files" &&
              !thread.outdated &&
              thread.anchor &&
              (!file || !matches(thread, file)),
            )}
          />
        ))}
        <Pagination trail={threadTrail} next={threads.data?.nextCursor} onPage={setThreadTrail} />
      </section>
    </section>
  );
}

function SelectedPatch({
  file,
  threads,
  webUrl,
  onReload,
  position,
  total,
  previous,
  next,
  pending,
  onPrevious,
  onNext,
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly webUrl: string;
  readonly onReload: () => void;
  readonly position: number;
  readonly total: number;
  readonly previous: boolean;
  readonly next: boolean;
  readonly pending: boolean;
  readonly onPrevious: () => void;
  readonly onNext: () => void;
}) {
  const source = safeUrl(webUrl);
  return (
    <section className="selected-patch" aria-label={`Patch for ${file.newPath}`}>
      <div className="spread patch-toolbar">
        <div>
          <strong>
            <code>{file.renamed ? `${file.oldPath} → ${file.newPath}` : file.newPath}</code>
          </strong>
          <DiffCount file={file} />
          <div className="actions patch-links">
            <span className="meta">
              {fileKind(file)}
              {file.lineCount !== null && ` · ${file.lineCount} rows`}
            </span>
            {file.availability === "complete" && (
              <a href={downloadPatchUrl(file)}>Download patch</a>
            )}
            {source && (
              <a href={`${source}/diffs`} target="_blank" rel="noreferrer">
                GitLab changes ↗
              </a>
            )}
          </div>
        </div>
        <div className="actions patch-arrows" aria-label="Files on this page">
          <button
            type="button"
            aria-label="Previous file"
            aria-disabled={pending || (position < 1 && !previous)}
            onClick={onPrevious}
          >
            ‹
          </button>
          <small>
            {position + 1} / {total}
          </small>
          <button
            type="button"
            aria-label="Next file"
            aria-disabled={pending || (position + 1 >= total && !next)}
            onClick={onNext}
          >
            ›
          </button>
        </div>
      </div>
      <PatchContents
        key={JSON.stringify([file.reviewId, file.revision, file.fileId])}
        file={file}
        threads={threads}
        onReload={onReload}
      />
    </section>
  );
}

function PatchContents({
  file,
  threads,
  onReload,
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly onReload: () => void;
}) {
  const [sourceMode, setSourceMode] = useState(!isRichPatch(file));
  const [window, setWindow] = useState<Window>({ offset: 0 });
  const [back, setBack] = useState<readonly Window[]>([]);
  const bookmark = `patch.scroll.${file.reviewId}.${file.revision}.${file.fileId}`;
  const patch = useResource(
    JSON.stringify([file.revision, file.fileId, sourceMode, window]),
    async (signal) => {
      if (file.availability !== "complete") return { cursor: "", data: null };
      try {
        return { cursor: "", data: await readPatch(file, signal, sourceMode ? window : undefined) };
      } catch (error) {
        if (
          !sourceMode &&
          !signal.aborted &&
          (error instanceof PatchFormattingLimit || error instanceof ResponseSizeLimit)
        )
          setSourceMode(true);
        throw error;
      }
    },
  );
  const body = patch.data;

  function navigate(target: Window) {
    setBack((items) => [...items, window].slice(-12));
    setWindow(target);
  }

  return (
    <>
      {file.availability !== "complete" ? (
        <Notice>{availability(file.availability)}</Notice>
      ) : (
        <>
          {patch.error && (
            <Notice retry={patch.retry}>
              {patch.error}{" "}
              <button type="button" onClick={onReload}>
                Reload file index
              </button>
            </Notice>
          )}
          {patch.pending && <Loading label="Loading selected patch…" />}
          {body?.text === "" && <Empty title="This file has an empty patch" />}
          {body &&
            (sourceMode ? (
              <>
                <div className="surface-card stack">
                  <div className="spread">
                    <strong>Large patch · source view</strong>
                    <span className="meta">
                      Bytes {body.first + 1}–{body.next} of {body.total}
                    </span>
                  </div>
                  <div className="actions">
                    <button
                      type="button"
                      disabled={!body.first}
                      onClick={() => navigate({ offset: 0, etag: body.etag })}
                    >
                      Start
                    </button>
                    <button
                      type="button"
                      disabled={!body.first}
                      onClick={() => {
                        const previous = back.at(-1);
                        setBack(back.slice(0, -1));
                        setWindow(
                          previous
                            ? { ...previous, etag: body.etag }
                            : {
                                offset: Math.max(0, body.first - PATCH_WINDOW_BYTES + 4),
                                etag: body.etag,
                              },
                        );
                      }}
                    >
                      Previous
                    </button>
                    <button
                      type="button"
                      disabled={body.next >= body.total}
                      onClick={() => navigate({ offset: body.next, etag: body.etag })}
                    >
                      Next
                    </button>
                    <button
                      type="button"
                      disabled={body.next >= body.total}
                      onClick={() => navigate({ offset: 0, last: true, etag: body.etag })}
                    >
                      End
                    </button>
                  </div>
                  <p className="meta">
                    Browser Find searches the displayed source range. A window can begin or end
                    mid-line.
                  </p>
                </div>
                <pre className="patch-source" tabIndex={0} aria-label="Patch source range">
                  {body.text}
                </pre>
              </>
            ) : (
              <PatchDocumentView text={body.text} discussions={threads} bookmark={bookmark} />
            ))}
        </>
      )}
      {(sourceMode || file.availability !== "complete" || (!body && patch.error)) &&
        threads.map((thread) => <DiscussionThread key={thread.discussionId} thread={thread} />)}
    </>
  );
}

function DiffCount({ file }: { readonly file: PatchFile }) {
  const { additions, deletions } = file;
  if (additions == null || deletions == null) return null;
  return (
    <small className="diff-count" aria-label={`${additions} additions, ${deletions} deletions`}>
      <span>+{additions}</span> <span>−{deletions}</span>
    </small>
  );
}

function matches(thread: PatchDiscussion, file: PatchFile) {
  return (
    !thread.outdated &&
    thread.anchor !== null &&
    thread.anchor.positionType === "text" &&
    thread.anchor.oldPath === file.oldPath &&
    thread.anchor.newPath === file.newPath &&
    (thread.anchor.newLine !== null || thread.anchor.oldLine !== null)
  );
}

function matchesFilters(thread: PatchDiscussion, status: Status, involving: boolean) {
  return (
    (!involving || thread.ourParticipation) && (status === "any" || thread.resolution === status)
  );
}

function isGeneral(thread: PatchDiscussion) {
  return !thread.anchor && !thread.outdated;
}

function fileKind(file: PatchFile) {
  if (file.deleted) return "Deleted";
  if (file.created) return "Added";
  return file.renamed ? "Renamed" : "Modified";
}

function availability(state: PatchFile["availability"]) {
  switch (state) {
    case "binary":
      return "This binary file has no text patch.";
    case "provider_limited":
      return "GitLab omitted the patch for this file.";
    case "incomplete":
      return "GitLab returned incomplete patch coverage.";
    default:
      return "The selected patch is unavailable.";
  }
}

function storedFile(reviewId: string): string | undefined {
  const value = savedValue(`changes.file.${reviewId}`);
  return typeof value === "string" ? value : undefined;
}
