import { useCallback, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { ApiResponseError } from "../../../api/client";
import { lostDiscussionAccess } from "../../../api/discussion-search";
import {
  PATCH_FILES_PER_PAGE,
  readInitialPatchFiles,
  readPatchDiscussions,
  readPatchFiles,
  readPatchPathCatalog,
  readPatchRevision,
} from "../../../api/changes";
import type { DiscussionDestination, PatchDiscussion } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Pagination } from "../../../components/Pagination";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { Approvals } from "./Approvals";
import { ConversationFilters, DEFAULT_FILTERS } from "./ConversationFilters";
import type { ConversationFilterValues } from "./ConversationFilters";
import { ChangesThread, ChangesThreadExpansion } from "./ChangesThread";
import { FileStream } from "./FileStream";
import { OtherDiscussions } from "./OtherDiscussions";
import { matchingFile, storedFile, threadLine } from "./patch-position";
import type { ThreadJump } from "./patch-position";
import { usePatchDiscussions } from "./usePatchDiscussions";
import { useDiscussionSelection } from "./useDiscussionSelection";
import { useDiscussionListJump } from "./useDiscussionListJump";
import "./changes.css";
import "./navigation.css";

type Tab = "files" | "general" | "all";
const DISCUSSIONS_PER_PAGE = 30;
const tabs = [
  ["files", "Files", "Files"],
  ["general", "General comments", "General"],
  ["all", "All conversations", "Conversations"],
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
  const [extraInline, setExtraInline] = useState<{
    readonly revision: string;
    readonly thread: PatchDiscussion;
  }>();
  const [loadedAnchors, setLoadedAnchors] = useState<ReadonlySet<string>>();
  const [tab, setTab] = useState<Tab>("files");
  const [filters, setFilters] = useState(DEFAULT_FILTERS);
  const { status, involving } = filters;
  const filterButton = useRef<HTMLButtonElement>(null);
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
  const note = useDiscussionSelection(reviewId, revision, openNote);
  const [discussionGeneration, setDiscussionGeneration] = useState(note.authority);
  if (note.authority !== discussionGeneration) {
    setDiscussionGeneration(note.authority);
    setThreadTrail([]);
    setThreadPage(0);
    setExtraInline(undefined);
    setThreadJump(undefined);
  }
  const list = useRef<HTMLElement>(null);
  const listTarget = tab === "all" ? note.selected : undefined;
  useDiscussionListJump(list, listTarget);
  const fallbackNote = useCallback(() => setTab("all"), []);
  const threads = usePatchDiscussions(reviewId, revision, note.authority);
  const confirmedPages = [...(threads.pages?.entries() ?? [])];
  const sharedThreadPage = confirmedPages[threadTrail.length]?.[1];
  const freshTrail = sharedThreadPage
    ? confirmedPages.slice(1, threadTrail.length + 1).flatMap(([cursor]) => cursor ?? [])
    : threadTrail;
  const threadCursor = freshTrail.at(-1);
  const requestThreadPage = Boolean(
    tab !== "files" && revision && !sharedThreadPage && !threads.pending,
  );
  const extraThreads = useResource(
    JSON.stringify([
      "discussion-page",
      reviewId,
      revision,
      requestThreadPage,
      threadCursor,
      note.authority,
    ]),
    (signal) =>
      requestThreadPage && revision
        ? readPatchDiscussions(reviewId, revision, signal, threadCursor)
        : Promise.resolve({ data: null, cursor: "" }),
    requestThreadPage ? [{ kind: "discussions", id: reviewId }] : [],
    lostDiscussionAccess,
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
  const counts = {
    files: totalFiles,
    general: discussionCount(filtered.filter(isGeneral).length, threads.limited),
    all: discussionCount(filtered.length, threads.limited),
  };
  const requestedInline = extraInline?.revision === revision ? extraInline?.thread : undefined;
  const inlineTarget = tab === "files" ? note.selected?.thread : undefined;
  const inline = useMemo(() => {
    if (inlineTarget)
      return [
        ...filtered.filter((thread) => thread.discussionId !== inlineTarget.discussionId),
        inlineTarget,
      ];
    return requestedInline &&
      matchesFilters(requestedInline, status, involving) &&
      !threads.items.some((item) => item.discussionId === requestedInline.discussionId)
      ? [...filtered, requestedInline]
      : filtered;
  }, [filtered, involving, inlineTarget, requestedInline, status, threads.items]);
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
            thread.discussionId !== listTarget?.thread.discussionId &&
            matchesFilters(thread, status, involving) &&
            (tab !== "general" || isGeneral(thread)),
        );

  const page = Math.min(
    threadPage,
    Math.max(0, Math.ceil(visible.length / DISCUSSIONS_PER_PAGE) - 1),
  );
  const displayed =
    tab === "files"
      ? visible.slice(page * DISCUSSIONS_PER_PAGE, (page + 1) * DISCUSSIONS_PER_PAGE)
      : listTarget
        ? [listTarget.thread, ...visible]
        : visible;

  function threadTarget(thread: PatchDiscussion): ThreadJump | undefined {
    const line = threadLine(thread);
    const anchor = thread.anchor;
    if (!line || !anchor) return;
    const file = shown?.items.find((entry) => matchingFile(thread, entry));
    if (file) return { fileId: file.fileId, line, thread };
    const path = catalog.data?.paths?.find((entry) => matchingFile(thread, entry));
    if (path) return { path, line, thread };
    if (catalog.data?.paths && !catalog.data.partial) return;
    return { paths: { oldPath: anchor.oldPath, newPath: anchor.newPath }, line, thread };
  }

  function openNote(destination: DiscussionDestination) {
    const target = threadTarget(destination.thread);
    if (target) {
      setThreadJump({ ...target, note: destination });
      setTab("files");
    } else setTab("all");
  }

  function refreshIndex() {
    note.clear();
    setThreadPage(0);
    setThreadTrail([]);
    setThreadJump(undefined);
    setExtraInline(undefined);
    setEpoch((value) => value + 1);
  }

  function changeFilters(next: ConversationFilterValues) {
    note.clear();
    setFilters(next);
    setThreadPage(0);
    setThreadTrail([]);
  }

  if (index.pending && !shown && !index.error) return <Loading label="Loading changes…" />;

  return (
    <ChangesThreadExpansion key={revision ?? reviewId} target={note.selected}>
      <section className="changes stack" aria-label="Changes">
        <div className="changes-top">
          <h3>Changes &amp; conversations</h3>
          <Approvals reviewId={reviewId} displayedSha={shown?.headSha} webUrl={webUrl} />
        </div>
        <div className="diff-revision">
          {shown && (
            <span title={`Base ${shown.baseSha}`}>
              Source <code title={shown.headSha}>{shown.headSha.slice(0, 12)}</code>
            </span>
          )}
          {children}
        </div>
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
          <div className="changes-tabs" role="group" aria-label="Changes view">
            {tabs.map(([id, label, compactLabel]) => (
              <button
                key={id}
                type="button"
                aria-pressed={tab === id}
                aria-label={[label, counts[id]].filter(Boolean).join(" ")}
                title={label}
                onClick={() => {
                  if (tab === id) return;
                  note.clear();
                  setThreadJump(undefined);
                  setTab(id);
                  setThreadPage(0);
                  setThreadTrail([]);
                }}
              >
                <span className="changes-tab-label">{label}</span>
                <span className="changes-tab-short" aria-hidden="true">
                  {compactLabel}
                </span>
                {counts[id] && <small className="changes-count">{counts[id]}</small>}
              </button>
            ))}
          </div>
          <ConversationFilters value={filters} onChange={changeFilters} trigger={filterButton} />
          {tab === "files" && shown && (
            <div className="changes-navigation-slot" ref={setNavigation} />
          )}
        </header>
        {index.error && <Notice retry={index.retry}>{index.error}</Notice>}
        {note.error && <Notice>{note.error}</Notice>}
        {tab === "files" && shown && (
          <FileStream
            key={`${shown.revision}:${epoch}`}
            reviewId={reviewId}
            initial={shown}
            catalog={catalog}
            threads={inline}
            webUrl={webUrl}
            toolbar={toolbar}
            navigation={navigation}
            threadJump={threadJump}
            onThreadJump={setThreadJump}
            onLoadedAnchors={setLoadedAnchors}
            restore={epoch === 0}
            onReload={refreshIndex}
            noteGeneration={note.generation}
            noteTarget={note.selected}
            onChooseNote={(hit) => void note.confirm(hit)}
            onClearNote={note.clear}
            onCancelNote={note.cancel}
            onNoteFallback={fallbackNote}
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
        <section className="discussion-list stack" aria-label="Review discussions" ref={list}>
          {tab === "files" && visible.length > 0 && <h3>Other discussions</h3>}
          {revision && (
            <p role="status" className="sr-only">
              {displayed.length} {displayed.length === 1 ? "discussion" : "discussions"} on this
              page
            </p>
          )}
          {tab !== "files" && !threadPending && !displayed.length && !extraThreads.error && (
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
                <button
                  type="button"
                  onClick={() => {
                    changeFilters(DEFAULT_FILTERS);
                    filterButton.current?.focus({ preventScroll: true });
                  }}
                >
                  Clear filters
                </button>
              )}
            </div>
          )}
          {tab === "files" && displayed.length > 0 && <OtherDiscussions threads={displayed} />}
          {tab !== "files" &&
            displayed.map((thread) => {
              const target = threadTarget(thread);
              return (
                <div key={thread.discussionId} className="changes-conversation">
                  <ChangesThread
                    thread={thread}
                    onJump={
                      tab === "all" && target
                        ? () => {
                            note.clear();
                            if (revision) setExtraInline({ revision, thread: target.thread });
                            setThreadJump(target);
                            setTab("files");
                          }
                        : undefined
                    }
                  />
                </div>
              );
            })}
          {tab !== "files" && (
            <Pagination trail={freshTrail} next={threadData?.nextCursor} onPage={setThreadTrail} />
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

function discussionCount(count: number, partial: boolean) {
  if (partial && count === 0) return undefined;
  return partial ? `${count}+` : String(count);
}

function matchesFilters(
  thread: PatchDiscussion,
  status: ConversationFilterValues["status"],
  involving: boolean,
) {
  return (
    (!involving || thread.ourParticipation) && (status === "any" || thread.resolution === status)
  );
}

function isGeneral(thread: PatchDiscussion) {
  return !thread.anchor && !thread.outdated;
}
