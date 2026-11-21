import { useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { ApiResponseError } from "../../../api/client";
import {
  PATCH_FILES_PER_PAGE,
  readInitialPatchFiles,
  readPatchDiscussions,
  readPatchFiles,
  readPatchPathCatalog,
  readPatchRevision,
} from "../../../api/changes";
import type { PatchDiscussion } from "../../../api/generated/contracts";
import { FilterSelect } from "../../../components/FilterSelect";
import { Icon } from "../../../components/Icon";
import { Pagination } from "../../../components/Pagination";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { Approvals } from "./Approvals";
import { ChangesThread, ChangesThreadExpansion } from "./ChangesThread";
import { FileStream } from "./FileStream";
import { matchingFile, storedFile, threadLine } from "./patch-position";
import type { ThreadJump } from "./patch-position";
import { usePatchDiscussions } from "./usePatchDiscussions";
import "./changes.css";
import "./navigation.css";

type Tab = "files" | "general" | "all";
type Status = "any" | "open" | "resolved";
const DISCUSSIONS_PER_PAGE = 30;
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
  const [epoch, setEpoch] = useState(0);
  const [navigation, setNavigation] = useState<HTMLDivElement | null>(null);
  const toolbar = useRef<HTMLDivElement>(null);
  const [threadJump, setThreadJump] = useState<ThreadJump>();
  const [loadedAnchors, setLoadedAnchors] = useState<ReadonlySet<string>>();
  const [tab, setTab] = useState<Tab>("files");
  const [status, setStatus] = useState<Status>("any");
  const [involving, setInvolving] = useState(false);
  const participant = useRef<HTMLSelectElement>(null);
  const index = useResource(JSON.stringify([reviewId, epoch]), async (signal) => {
    try {
      return epoch > 0
        ? await readPatchFiles(reviewId, signal)
        : await readInitialPatchFiles(reviewId, signal, storedFile(reviewId));
    } catch (error) {
      if (
        signal.aborted ||
        epoch > 0 ||
        !storedFile(reviewId) ||
        !(error instanceof ApiResponseError)
      )
        throw error;
      return readPatchFiles(reviewId, signal);
    }
  });
  const shown = index.data;
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
  const [threadPage, setThreadPage] = useState(0);
  const [threadTrail, setThreadTrail] = useState<readonly string[]>([]);
  const filtering = involving || status !== "any";
  const threads = usePatchDiscussions(reviewId, revision);
  const threadCursor = threadTrail.at(-1);
  const sharedThreadPage = threads.pages?.get(threadCursor);
  const requestThreadPage = Boolean(
    tab !== "files" && revision && !sharedThreadPage && !threads.pending,
  );
  const extraThreads = useResource(
    JSON.stringify(["discussion-page", reviewId, revision, requestThreadPage, threadCursor]),
    (signal) =>
      requestThreadPage && revision
        ? readPatchDiscussions(reviewId, revision, signal, threadCursor)
        : Promise.resolve({ data: null, cursor: "" }),
    requestThreadPage ? [{ kind: "discussions", id: reviewId }] : [],
  );
  const threadData = sharedThreadPage?.data ?? extraThreads.data;
  const threadPending =
    tab === "files" ? threads.pending : !threadData && (threads.pending || extraThreads.pending);
  const filtered = useMemo(
    () => threads.items.filter((thread) => matchesFilters(thread, status, involving)),
    [threads.items, status, involving],
  );
  const knownFiles = shown ? (shown.pageNumber - 1) * PATCH_FILES_PER_PAGE + shown.items.length : 0;
  const totalFiles = catalog.data
    ? `${Math.max(knownFiles, catalog.data.count)}${catalog.data.partial ? "+" : ""}`
    : shown
      ? `${knownFiles}${shown.nextCursor ? "+" : ""}`
      : undefined;
  const general = filtered.filter(isGeneral);
  const visible =
    tab === "files"
      ? filtered.filter(
          (thread) =>
            !loadedAnchors?.has(thread.discussionId) &&
            !catalog.data?.paths?.some((path) => matchingFile(thread, path)) &&
            !shown?.items.some((file) => matchingFile(thread, file)),
        )
      : (threadData?.items ?? []).filter(
          (thread) =>
            matchesFilters(thread, status, involving) && (tab !== "general" || isGeneral(thread)),
        );

  const page = Math.min(
    threadPage,
    Math.max(0, Math.ceil(visible.length / DISCUSSIONS_PER_PAGE) - 1),
  );
  const displayed =
    tab === "files"
      ? visible.slice(page * DISCUSSIONS_PER_PAGE, (page + 1) * DISCUSSIONS_PER_PAGE)
      : visible;

  function refreshIndex() {
    setThreadPage(0);
    setThreadTrail([]);
    setThreadJump(undefined);
    setEpoch((value) => value + 1);
  }

  function clearConversationFilters() {
    setInvolving(false);
    setStatus("any");
    setThreadPage(0);
    setThreadTrail([]);
    participant.current?.focus({ preventScroll: true });
  }

  if (index.pending && !shown && !index.error) return <Loading label="Loading changes…" />;

  return (
    <ChangesThreadExpansion key={revision ?? reviewId}>
      <section className="changes stack" aria-label="Changes">
        <div className="changes-top">
          <h3>Changes &amp; conversations</h3>
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
            The source revision changed. The displayed patch remains on{" "}
            {shown?.headSha.slice(0, 12)}.
            <button type="button" onClick={refreshIndex}>
              Load current changes
            </button>
          </Notice>
        )}
        <header className="changes-header" ref={toolbar}>
          <div className="thread-filters" role="group" aria-label="Conversation filters">
            <span className="changes-filter-title" aria-hidden="true">
              Conversation filters
            </span>
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
                setThreadPage(0);
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
                setThreadPage(0);
                setThreadTrail([]);
              }}
            />
            {filtering && (
              <button
                type="button"
                className="changes-clear"
                aria-label="Clear conversation filters"
                onClick={clearConversationFilters}
              >
                Clear
              </button>
            )}
          </div>
          <div className="changes-navigation">
            <div className="changes-tabs" role="group" aria-label="Changes view">
              {tabs.map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  aria-pressed={tab === id}
                  onClick={() => {
                    if (tab === id) return;
                    setThreadJump(undefined);
                    setTab(id);
                    setThreadPage(0);
                    setThreadTrail([]);
                  }}
                >
                  {label}
                  <small className="changes-count">
                    {id === "files"
                      ? totalFiles
                      : `${id === "general" ? general.length : filtered.length}${threads.limited ? "+" : ""}`}
                  </small>
                </button>
              ))}
            </div>
            {tab === "files" && shown && (
              <div className="changes-navigation-slot" ref={setNavigation} />
            )}
          </div>
        </header>
        {index.error && <Notice retry={index.retry}>{index.error}</Notice>}
        {tab === "files" && shown && (
          <FileStream
            key={`${shown.revision}:${epoch}`}
            reviewId={reviewId}
            initial={shown}
            catalog={catalog}
            threads={filtered}
            webUrl={webUrl}
            toolbar={toolbar}
            navigation={navigation}
            threadJump={threadJump}
            onThreadJump={setThreadJump}
            onLoadedAnchors={setLoadedAnchors}
            restore={epoch === 0}
            onReload={refreshIndex}
          />
        )}
        {threads.error && <Notice retry={threads.retry}>{threads.error}</Notice>}
        {extraThreads.error && <Notice retry={extraThreads.retry}>{extraThreads.error}</Notice>}
        {threadPending && (
          <p className="meta" role="status">
            Loading more conversations…
          </p>
        )}
        {tab === "files" && !threads.pending && threads.limited && !threads.error && (
          <p className="meta" role="status">
            Inline conversation coverage is partial. All conversations has more pages.
          </p>
        )}
        <section className="discussion-list stack" aria-label="Review discussions">
          {tab === "files" && visible.length > 0 && <h3>Other discussions</h3>}
          {revision && (
            <p role="status" className="sr-only">
              {visible.length} {visible.length === 1 ? "discussion" : "discussions"} on this page
            </p>
          )}
          {tab !== "files" && !threadPending && !visible.length && !extraThreads.error && (
            <div className="changes-conversation-empty">
              <div role="status">
                <Icon name="inbox" size={28} />
                <h3>
                  {threadData?.nextCursor
                    ? "No matching conversations on this page"
                    : filtering
                      ? tab === "general"
                        ? "No matching comments"
                        : "No matching conversations"
                      : tab === "general"
                        ? "No general comments yet"
                        : "No conversations yet"}
                </h3>
                {filtering && <p className="meta">Try changing your filters.</p>}
              </div>
              {filtering && (
                <button type="button" onClick={clearConversationFilters}>
                  Clear filters
                </button>
              )}
            </div>
          )}
          {displayed.map((thread) => {
            const line = threadLine(thread);
            const path = line && catalog.data?.paths?.find((entry) => matchingFile(thread, entry));
            const file = line && shown?.items.find((entry) => matchingFile(thread, entry));
            const anchor = thread.anchor;
            const target: ThreadJump | undefined =
              line && anchor
                ? file
                  ? { fileId: file.fileId, line }
                  : path
                    ? { path, line }
                    : { paths: { oldPath: anchor.oldPath, newPath: anchor.newPath }, line }
                : undefined;
            return (
              <div key={thread.discussionId} className="changes-conversation">
                <ChangesThread
                  thread={thread}
                  outside={tab === "files" && !thread.outdated && Boolean(thread.anchor)}
                />
                {tab === "all" && target && (
                  <button
                    type="button"
                    className="changes-clear"
                    onClick={() => {
                      setThreadJump(target);
                      setTab("files");
                    }}
                  >
                    Jump to line
                  </button>
                )}
              </div>
            );
          })}
          {tab !== "files" && (
            <Pagination trail={threadTrail} next={threadData?.nextCursor} onPage={setThreadTrail} />
          )}
          {tab === "files" && visible.length > DISCUSSIONS_PER_PAGE && (
            <nav className="pagination" aria-label="Conversation pages">
              <button type="button" disabled={page === 0} onClick={() => setThreadPage(page - 1)}>
                Previous
              </button>
              <span className="meta">Page {page + 1}</span>
              <button
                type="button"
                disabled={(page + 1) * DISCUSSIONS_PER_PAGE >= visible.length}
                onClick={() => setThreadPage(page + 1)}
              >
                Next
              </button>
            </nav>
          )}
        </section>
      </section>
    </ChangesThreadExpansion>
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
