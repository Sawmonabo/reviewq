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
import { DiscussionThread } from "../DiscussionThread";
import { Approvals } from "./Approvals";
import { FileStream } from "./FileStream";
import { matchingFile, storedFile } from "./patch-position";
import "./changes.css";

type Tab = "files" | "general" | "all";
type Status = "any" | "open" | "resolved";
const MAX_EMPTY_DISCUSSION_PAGES = 4;
const EMPTY_DISCUSSIONS: readonly PatchDiscussion[] = [];
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
  const [query, setQuery] = useState("");
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
  const discussionItems = threads.data?.items ?? EMPTY_DISCUSSIONS;
  const filtered = useMemo(
    () => discussionItems.filter((thread) => matchesFilters(thread, status, involving)),
    [discussionItems, status, involving],
  );
  const knownFiles = shown ? (shown.pageNumber - 1) * PATCH_FILES_PER_PAGE + shown.items.length : 0;
  const totalFiles = catalog.data
    ? `${Math.max(knownFiles, catalog.data.count)}${catalog.data.partial ? "+" : ""}`
    : shown
      ? `${knownFiles}${shown.nextCursor ? "+" : ""}`
      : undefined;
  const general = filtered.filter(isGeneral);
  const visible =
    tab === "general"
      ? general
      : tab === "files"
        ? filtered.filter(
            (thread) =>
              !loadedAnchors?.has(thread.discussionId) &&
              !catalog.data?.paths?.some((path) => matchingFile(thread, path)) &&
              !shown?.items.some((file) => matchingFile(thread, file)),
          )
        : filtered;

  function refreshIndex() {
    setThreadTrail([]);
    setEpoch((value) => value + 1);
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
      {index.error && <Notice retry={index.retry}>{index.error}</Notice>}
      {tab === "files" && shown && (
        <FileStream
          key={`${shown.revision}:${epoch}`}
          reviewId={reviewId}
          initial={shown}
          catalog={catalog}
          threads={filtered}
          webUrl={webUrl}
          query={query}
          onQuery={setQuery}
          onLoadedAnchors={setLoadedAnchors}
          restore={epoch === 0}
          onReload={refreshIndex}
        />
      )}
      {threads.error && <Notice retry={threads.retry}>{threads.error}</Notice>}
      {threads.pending && <Loading label="Loading discussions…" />}
      <section className="discussion-list stack" aria-label="Review discussions">
        {tab === "files" && visible.length > 0 && <h3>Other discussions</h3>}
        {threads.data && (
          <p role="status" className="sr-only">
            {visible.length} {visible.length === 1 ? "discussion" : "discussions"} on this page
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
            outside={tab === "files" && !thread.outdated && Boolean(thread.anchor)}
          />
        ))}
        <Pagination trail={threadTrail} next={threads.data?.nextCursor} onPage={setThreadTrail} />
      </section>
    </section>
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
