import { useCallback, useRef, useState } from "react";
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
import type { PatchDiscussion, PatchFile } from "../../../api/generated/contracts";
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

type Position = { readonly revision: string; readonly cursor: string };
type Tab = "files" | "general" | "all";
type Status = "any" | "open" | "resolved";
type Window = { readonly offset: number; readonly etag?: string; readonly last?: boolean };
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
  const [positions, setPositions] = useState<readonly Position[]>([]);
  const [epoch, setEpoch] = useState(0);
  const [frozenRevision, setFrozenRevision] = useState<string>();
  const [tab, setTab] = useState<Tab>("files");
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<Status>("any");
  const [involving, setInvolving] = useState(false);
  const participant = useRef<HTMLSelectElement>(null);
  const indexKey = JSON.stringify([reviewId, epoch, positions.at(-1)]);
  const index = useResource(indexKey, async (signal) => {
    const position = positions.at(-1);
    try {
      return await readPatchFiles(
        reviewId,
        signal,
        position,
        position || epoch > 0 ? undefined : storedFile(reviewId),
      );
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
  const revision = index.data?.revision ?? frozenRevision;
  const current = useResource(
    `change-current:${reviewId}`,
    (signal) => readPatchFiles(reviewId, signal),
    [{ kind: "changes", id: reviewId }],
  );
  const changed = Boolean(revision && current.data && current.data.revision !== revision);
  const [threadTrail, setThreadTrail] = useState<readonly string[]>([]);
  const threads = useResource(
    `discussions:${reviewId}:${revision ?? ""}:${threadTrail.at(-1) ?? ""}`,
    (signal) =>
      revision
        ? readPatchDiscussions(reviewId, revision, signal, threadTrail.at(-1))
        : Promise.resolve({ data: null, cursor: "" }),
    [{ kind: "discussions", id: reviewId }],
  );
  const available =
    index.data?.items.filter((file) =>
      `${file.newPath} ${file.oldPath}`.toLowerCase().includes(query.toLowerCase()),
    ) ?? [];
  const files = index.data?.items ?? [];
  const file = files.find((entry) => entry.fileId === fileId) ?? files[0];
  const filePosition = file ? files.indexOf(file) : -1;
  const fileSearch = useRef<HTMLInputElement>(null);
  const picker = useRef<HTMLDetailsElement>(null);
  const filtered = (threads.data?.items ?? []).filter(
    (thread) =>
      (!involving || thread.ourParticipation) && (status === "any" || thread.resolution === status),
  );
  const anchors = file ? filtered.filter((thread) => matches(thread, file)) : [];
  const visible =
    tab === "files"
      ? filtered.filter((thread) => !file || !matches(thread, file))
      : tab === "general"
        ? filtered.filter((thread) => !thread.anchor)
        : filtered;

  function selectFile(next: PatchFile) {
    setFileId(next.fileId);
    saveValue(`changes.file.${reviewId}`, next.fileId);
  }

  function refreshIndex() {
    setPositions([]);
    setFrozenRevision(undefined);
    setThreadTrail([]);
    setEpoch((value) => value + 1);
  }

  if (index.pending && !frozenRevision && !index.error) return <Loading label="Loading changes…" />;

  return (
    <section className="changes stack" aria-label="Changes">
      <div className="changes-top">
        <div>
          <h3>Changes &amp; conversations</h3>
          {index.data && (
            <p className="meta">
              {files.length}
              {index.data.nextCursor ? "+" : ""} files · {filtered.length}
              {threads.data?.nextCursor ? "+" : ""} conversations
            </p>
          )}
        </div>
        <Approvals reviewId={reviewId} displayedSha={index.data?.headSha} />
      </div>
      {children}
      {index.data && (
        <div className="diff-revision">
          <span>
            Source <code>{index.data.headSha.slice(0, 12)}</code>
          </span>
          <span>
            Base <code>{index.data.baseSha.slice(0, 12)}</code>
          </span>
        </div>
      )}
      {changed && (
        <Notice>
          The source revision changed. The displayed patch remains on{" "}
          {index.data?.headSha.slice(0, 12)}.
          <button type="button" onClick={refreshIndex}>
            Load current changes
          </button>
        </Notice>
      )}
      <div className="actions changes-tabs">
        {tabs.map(([id, label]) => (
          <button key={id} type="button" aria-pressed={tab === id} onClick={() => setTab(id)}>
            {label}{" "}
            <strong className="changes-count">
              {id === "files"
                ? files.length
                : id === "general"
                  ? filtered.filter((thread) => !thread.anchor).length
                  : filtered.length}
              {(id === "files" ? index.data?.nextCursor : threads.data?.nextCursor) && "+"}
            </strong>
          </button>
        ))}
      </div>
      <div className="actions thread-filters">
        <label>
          Show{" "}
          <select
            ref={participant}
            value={involving ? "mine" : "all"}
            onChange={(event) => setInvolving(event.currentTarget.value === "mine")}
          >
            <option value="all">Everyone</option>
            <option value="mine">Involving you</option>
          </select>
        </label>
        <label>
          {" "}
          Status{" "}
          <select
            value={status}
            onChange={(event) => setStatus(event.currentTarget.value as Status)}
          >
            <option value="any">Any status</option>
            <option value="open">Open</option>
            <option value="resolved">Resolved</option>
          </select>
        </label>
        {(involving || status !== "any") && (
          <button
            type="button"
            onClick={() => {
              setInvolving(false);
              setStatus("any");
              participant.current?.focus();
            }}
          >
            Clear conversation filters
          </button>
        )}
      </div>
      {index.error && (
        <Notice retry={index.retry}>
          {index.error}{" "}
          {positions.length > 0 && (
            <button type="button" onClick={refreshIndex}>
              Restart file list
            </button>
          )}
        </Notice>
      )}
      {tab === "files" && (
        <>
          {index.pending && <Loading label="Loading changed files…" />}
          {index.data && (
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
                    {index.data.nextCursor ? "+" : ""}
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
                <nav aria-label="Changed files">
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
                      {item.lineCount != null && <small>{item.lineCount} rows</small>}
                    </button>
                  ))}
                  {!available.length && (
                    <p className="meta">
                      {query ? "No matching files on this page." : "No changed files."}
                    </p>
                  )}
                </nav>
                <Pagination
                  trail={positions.map((position) => position.cursor)}
                  next={index.data.nextCursor}
                  onPage={(trail) => {
                    const shown = index.data;
                    if (!shown) return;
                    setFrozenRevision(shown.revision);
                    setPositions(trail.map((cursor) => ({ revision: shown.revision, cursor })));
                  }}
                />
              </details>
              {file && (
                <SelectedPatch
                  key={JSON.stringify([file.reviewId, file.revision, file.fileId])}
                  file={file}
                  threads={anchors}
                  webUrl={webUrl}
                  onReload={refreshIndex}
                  position={filePosition}
                  total={files.length}
                  onPrevious={() => {
                    const next = files[filePosition - 1];
                    if (next) selectFile(next);
                  }}
                  onNext={() => {
                    const next = files[filePosition + 1];
                    if (next) selectFile(next);
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
          <p className="meta">
            {involving || status !== "any"
              ? "No conversations match the filters."
              : "No conversations in this view."}
          </p>
        )}
        {visible.map((thread) => (
          <DiscussionThread key={thread.discussionId} thread={thread} />
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
  onPrevious,
  onNext,
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly webUrl: string;
  readonly onReload: () => void;
  readonly position: number;
  readonly total: number;
  readonly onPrevious: () => void;
  readonly onNext: () => void;
}) {
  const [sourceMode, setSourceMode] = useState(!isRichPatch(file));
  const [window, setWindow] = useState<Window>({ offset: 0 });
  const [back, setBack] = useState<readonly Window[]>([]);
  const bookmark = `patch.scroll.${file.reviewId}.${file.revision}.${file.fileId}`;
  const restoreScroll = useCallback(() => {
    const saved = savedValue(bookmark);
    return typeof saved === "number" && saved >= 0 && Number.isFinite(saved) ? saved : 0;
  }, [bookmark]);
  const saveScroll = useCallback((scroll: number) => saveValue(bookmark, scroll), [bookmark]);
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
  const source = safeUrl(webUrl);

  function navigate(target: Window) {
    setBack((items) => [...items, window].slice(-12));
    setWindow(target);
  }

  return (
    <section className="selected-patch" aria-label={`Patch for ${file.newPath}`}>
      <div className="spread patch-toolbar">
        <div>
          <strong>
            <code>{file.renamed ? `${file.oldPath} → ${file.newPath}` : file.newPath}</code>
          </strong>
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
            disabled={position < 1}
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
            disabled={position + 1 >= total}
            onClick={onNext}
          >
            ›
          </button>
        </div>
      </div>
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
              <PatchDocumentView
                text={body.text}
                discussions={threads}
                restoreScroll={restoreScroll}
                rememberScroll={saveScroll}
              />
            ))}
        </>
      )}
      {(sourceMode || file.availability !== "complete" || (!body && patch.error)) &&
        threads.map((thread) => <DiscussionThread key={thread.discussionId} thread={thread} />)}
    </section>
  );
}

function matches(thread: PatchDiscussion, file: PatchFile) {
  return (
    !thread.outdated &&
    thread.anchor !== null &&
    thread.anchor.oldPath === file.oldPath &&
    thread.anchor.newPath === file.newPath &&
    (thread.anchor.newLine !== null || thread.anchor.oldLine !== null)
  );
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
