import { useRef, useState } from "react";
import type { ReactNode } from "react";
import { ResponseSizeLimit } from "../../../api/bounded-bytes";
import { ApiResponseError } from "../../../api/client";
import {
  downloadPatchUrl,
  isRichPatch,
  PATCH_FILES_PER_PAGE,
  PATCH_WINDOW_BYTES,
  PatchFormattingLimit,
  readInitialPatchFiles,
  readPatch,
  readPatchDiscussions,
  readPatchFiles,
  readPatchPathCatalog,
  readPatchRevision,
} from "../../../api/changes";
import type {
  PatchDiscussion,
  PatchFile,
  PatchFileIndex,
  PatchPath,
} from "../../../api/generated/contracts";
import { FilterSelect } from "../../../components/FilterSelect";
import { Icon } from "../../../components/Icon";
import { Pagination } from "../../../components/Pagination";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { safeUrl } from "../../../lib/presentation";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { DiscussionThread } from "../DiscussionThread";
import { Approvals } from "./Approvals";
import { ChangedFilePicker, DiffCount, FileKind } from "./ChangedFilePicker";
import { PatchDocumentView } from "./PatchDocumentView";
import "./changes.css";

type Boundary = "first" | "last";
type Position = { readonly revision: string } & (
  { readonly cursor: string; readonly boundary?: Boundary } | { readonly path: PatchPath }
);
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
  const [fileQuery, setFileQuery] = useState("");
  const [status, setStatus] = useState<Status>("any");
  const [involving, setInvolving] = useState(false);
  const participant = useRef<HTMLSelectElement>(null);
  const indexKey = JSON.stringify([reviewId, epoch, position]);
  const index = useResource(indexKey, async (signal) => {
    try {
      const result =
        position || epoch > 0
          ? await readPatchFiles(reviewId, signal, position)
          : await readInitialPatchFiles(reviewId, signal, storedFile(reviewId));
      if (position && !signal.aborted) {
        if ("path" in position) {
          const path = position.path;
          const selected = result.data.items.find(
            (file) => file.oldPath === path.oldPath && file.newPath === path.newPath,
          );
          if (!selected) throw new Error("The file catalog and patch index disagree.");
          selectFile(selected);
        } else if (position.boundary) {
          const selected =
            position.boundary === "last" ? result.data.items.at(-1) : result.data.items[0];
          if (selected) selectFile(selected);
        }
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
  const catalog = useResource(
    JSON.stringify(["file-catalog", reviewId, revision, epoch]),
    (signal) =>
      revision
        ? readPatchPathCatalog(reviewId, revision, signal)
        : Promise.resolve({ data: null, cursor: "" }),
  );
  const current = useResource(
    `change-current:${reviewId}:${epoch}`,
    (signal) =>
      revision ? readPatchRevision(reviewId, signal) : Promise.resolve({ data: null, cursor: "" }),
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
  const files = shown?.items ?? [];
  const file = files.find((entry) => entry.fileId === fileId) ?? files[0];
  const filePosition = file ? files.indexOf(file) : -1;
  const pageNumber = shown?.pageNumber ?? 1;
  const pageOffset = (pageNumber - 1) * PATCH_FILES_PER_PAGE;
  const lastKnownFile = pageOffset + files.length;
  const totalFiles = catalog.data
    ? `${Math.max(catalog.data.count, lastKnownFile)}${catalog.data.partial ? "+" : ""}`
    : shown && (!shown.nextCursor || catalog.error)
      ? `${lastKnownFile}${shown.nextCursor ? "+" : ""}`
      : undefined;
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

  function choosePath(path: PatchPath) {
    if (!index.data || index.pending) return;
    const found = index.data.items.find(
      (file) => file.oldPath === path.oldPath && file.newPath === path.newPath,
    );
    if (found) {
      selectFile(found);
      return;
    }
    setRetained(index.data);
    setPosition({ revision: index.data.revision, path });
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
          {(totalFiles !== undefined || threads.data) && (
            <p className="meta">
              {totalFiles !== undefined && `${totalFiles} files`}
              {totalFiles !== undefined && threads.data && " · "}
              {threads.data &&
                `${filtered.length}${threads.data.nextCursor || threadTrail.length ? "+" : ""} conversations`}
            </p>
          )}
        </div>
        <Approvals reviewId={reviewId} displayedSha={shown?.headSha} webUrl={webUrl} />
      </div>
      {children}
      {shown && (
        <div className="diff-revision">
          <span title={`Base ${shown.baseSha}`}>
            Source <code title={shown.headSha}>{shown.headSha.slice(0, 12)}</code>
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
            {id === "files" ? (
              <strong className="changes-count">{totalFiles}</strong>
            ) : (
              threads.data &&
              (id === "general" || discussionScope === "all") && (
                <strong className="changes-count">
                  {id === "general" ? general.length : filtered.length}
                  {(threads.data.nextCursor || threadTrail.length > 0) && "+"}
                </strong>
              )
            )}
          </button>
        ))}
      </div>
      <div className="actions thread-filters">
        <FilterSelect
          label="Show"
          accessibleLabel="Conversation participants"
          selectRef={participant}
          value={involving ? "mine" : "all"}
          options={[
            { value: "all", label: "Everyone" },
            { value: "mine", label: "Involving you" },
          ]}
          onChange={(value) => {
            setInvolving(value === "mine");
            setThreadTrail([]);
          }}
        />
        <FilterSelect
          label="Status"
          accessibleLabel="Conversation status"
          value={status}
          options={[
            { value: "any", label: "Any status" },
            { value: "open", label: "Open only" },
            { value: "resolved", label: "Resolved only" },
          ]}
          onChange={(value) => {
            setStatus(value as Status);
            setThreadTrail([]);
          }}
        />
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
              <ChangedFilePicker
                reviewId={reviewId}
                index={shown}
                selected={file}
                total={totalFiles}
                pending={index.pending}
                catalog={catalog}
                query={fileQuery}
                onQuery={setFileQuery}
                onChooseFile={(file) => {
                  if (!index.pending) selectFile(file);
                }}
                onChoosePath={choosePath}
                onPage={movePage}
              />
              {file && (
                <SelectedPatch
                  file={file}
                  threads={anchors}
                  webUrl={webUrl}
                  onReload={refreshIndex}
                  position={pageOffset + filePosition + 1}
                  total={totalFiles}
                  previous={filePosition > 0 || Boolean(shown.previousCursor)}
                  next={filePosition + 1 < files.length || Boolean(shown.nextCursor)}
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
        {tab === "files" && visible.length > 0 && <h3>Other discussions</h3>}
        {threads.data && (
          <p role="status" className="sr-only">
            {visible.length} {visible.length === 1 ? "discussion" : "discussions"} on this page
            {tab === "files" && anchors.length > 0 ? ` · ${anchors.length} on this file` : ""}
          </p>
        )}
        {tab !== "files" && threads.data && !visible.length && (
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
  readonly total: string | undefined;
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
        <div className="patch-file-title">
          <strong>
            <code>{file.renamed ? `${file.oldPath} → ${file.newPath}` : file.newPath}</code>
          </strong>
          <div className="actions patch-links">
            <span className="meta">
              <FileKind file={file} />
            </span>
            <DiffCount file={file} />
            {file.availability === "complete" && (
              <a href={downloadPatchUrl(file)} aria-label="Download patch" title="Download patch">
                <Icon name="download" size={14} />
              </a>
            )}
            {source && (
              <a
                href={`${source}/diffs`}
                target="_blank"
                rel="noreferrer"
                aria-label="Open changes in GitLab"
                title="Open changes in GitLab"
              >
                <Icon name="external" size={13} />
              </a>
            )}
          </div>
        </div>
        <div className="actions patch-arrows" aria-label="Changed file position">
          <button
            type="button"
            aria-label="Previous file"
            aria-disabled={pending || !previous}
            onClick={onPrevious}
          >
            ‹
          </button>
          <small>{total === undefined ? `File ${position}` : `${position}/${total}`}</small>
          <button
            type="button"
            aria-label="Next file"
            aria-disabled={pending || !next}
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
