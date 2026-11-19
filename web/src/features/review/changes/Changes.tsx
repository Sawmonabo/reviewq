import { useCallback, useRef, useState } from "react";
import { ResponseSizeLimit } from "../../../api/bounded-bytes";
import { ApiResponseError } from "../../../api/client";
import {
  downloadPatchUrl,
  isRichPatch,
  PATCH_WINDOW_BYTES,
  PatchFormattingLimit,
  readApprovals,
  readPatch,
  readPatchDiscussions,
  readPatchFiles,
} from "../../../api/changes";
import type { PatchDiscussion, PatchFile } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Pagination } from "../../../components/Pagination";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { formatTime, safeUrl } from "../../../lib/presentation";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { DiscussionThread } from "../DiscussionThread";
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
}: {
  readonly reviewId: string;
  readonly webUrl: string;
}) {
  const [fileId, setFileId] = useState(() => storedFile(reviewId));
  const [positions, setPositions] = useState<readonly Position[]>([]);
  const [epoch, setEpoch] = useState(0);
  const [frozenRevision, setFrozenRevision] = useState<string>();
  const [tab, setTab] = useState<Tab>("files");
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<Status>("any");
  const [involving, setInvolving] = useState(false);
  const participant = useRef<HTMLInputElement>(null);
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
  const file = available.find((entry) => entry.fileId === fileId) ?? available[0];
  const filtered = (threads.data?.items ?? []).filter(
    (thread) =>
      (!involving || thread.ourParticipation) &&
      (status === "any" || (thread.notes[0]?.resolved === true) === (status === "resolved")),
  );
  const anchors = file ? filtered.filter((thread) => matches(thread, file)) : [];
  const visible =
    tab === "files"
      ? filtered.filter((thread) => !file || !matches(thread, file))
      : tab === "general"
        ? filtered.filter((thread) => !thread.anchor)
        : filtered;

  function refreshIndex() {
    setPositions([]);
    setFrozenRevision(undefined);
    setThreadTrail([]);
    setEpoch((value) => value + 1);
  }

  return (
    <section className="changes stack" aria-label="Changes">
      <Approvals reviewId={reviewId} />
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
            {label}
          </button>
        ))}
      </div>
      <div className="actions thread-filters">
        <label>
          <input
            ref={participant}
            type="checkbox"
            checked={involving}
            onChange={(event) => setInvolving(event.currentTarget.checked)}
          />{" "}
          Involving you
        </label>
        <label>
          {" "}
          Status{" "}
          <select
            value={status}
            onChange={(event) => setStatus(event.currentTarget.value as Status)}
          >
            <option value="any">Any</option>
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
              <div className="stack">
                <label className="search-field">
                  <Icon name="search" />
                  <input
                    aria-label="Search files on this page"
                    value={query}
                    placeholder="Find changed file"
                    onChange={(event) => setQuery(event.currentTarget.value)}
                  />
                </label>
                {available.length > 0 && (
                  <label className="field">
                    Selected file
                    <select
                      value={file?.fileId ?? ""}
                      onChange={(event) => {
                        setFileId(event.currentTarget.value);
                        saveValue(`changes.file.${reviewId}`, event.currentTarget.value);
                      }}
                    >
                      {available.map((file) => (
                        <option key={file.fileId} value={file.fileId}>
                          {file.renamed ? `${file.oldPath} → ${file.newPath}` : file.newPath}
                          {file.created ? " · Added" : file.deleted ? " · Deleted" : ""}
                          {file.availability !== "complete" ? ` · ${file.availability}` : ""}
                        </option>
                      ))}
                    </select>
                  </label>
                )}
                {!available.length && (
                  <Empty title={query ? "No matching files on this page" : "No changed files"} />
                )}
                <Pagination
                  trail={positions.map((position) => position.cursor)}
                  next={index.data.nextCursor}
                  onPage={(trail) => {
                    if (!index.data) return;
                    setFrozenRevision(index.data.revision);
                    setPositions(
                      trail.map((cursor) => ({ revision: index.data?.revision ?? "", cursor })),
                    );
                  }}
                />
              </div>
              {file && (
                <SelectedPatch
                  key={JSON.stringify([file.reviewId, file.revision, file.fileId])}
                  file={file}
                  threads={anchors}
                  webUrl={webUrl}
                  onReload={refreshIndex}
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
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly webUrl: string;
  readonly onReload: () => void;
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
    <section className="stack" aria-label={`Patch for ${file.newPath}`}>
      <div className="spread patch-toolbar">
        <strong>
          <code>{file.newPath}</code>
        </strong>
        <div className="actions">
          {file.created && <span className="badge green">Added</span>}
          {file.deleted && <span className="badge red">Deleted</span>}
          {file.lineCount !== null && <span className="meta">{file.lineCount} rows</span>}
          {file.availability === "complete" && <a href={downloadPatchUrl(file)}>Download patch</a>}
          {source && (
            <a href={`${source}/diffs`} target="_blank" rel="noreferrer">
              GitLab changes ↗
            </a>
          )}
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
      {(sourceMode || file.availability !== "complete") &&
        threads.map((thread) => <DiscussionThread key={thread.discussionId} thread={thread} />)}
    </section>
  );
}

function Approvals({ reviewId }: { readonly reviewId: string }) {
  const [open, setOpen] = useState(false);
  return (
    <details
      className="surface-card approval-card"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>Approval requirements</summary>
      {open && <ApprovalDetail reviewId={reviewId} />}
    </details>
  );
}

function ApprovalDetail({ reviewId }: { readonly reviewId: string }) {
  const approvals = useResource(
    `approvals:${reviewId}`,
    (signal) => readApprovals(reviewId, signal),
    [{ kind: "review", id: reviewId }],
  );
  const value = approvals.data;
  return (
    <div className="stack">
      {approvals.error && <Notice retry={approvals.retry}>{approvals.error}</Notice>}
      {approvals.pending && <Loading label="Loading approval requirements…" />}
      {value && (
        <>
          <p className="meta">
            Last checked {formatTime(value.observedAt)} · {value.sourceSha.slice(0, 12)}
          </p>
          <p>
            {value.personalApproval === null
              ? "Your approval is unknown"
              : value.personalApproval
                ? "You approved this MR"
                : "You have not approved this MR"}
          </p>
          {value.approvals?.left !== null && value.approvals?.left !== undefined && (
            <p>{value.approvals.left} more approvals needed</p>
          )}
          {value.approvals?.approvers.map((approver) => (
            <span key={approver.id}>
              {approver.username ?? approver.id}
              {approver.approvedAt && ` · ${formatTime(approver.approvedAt)}`}
            </span>
          ))}
          {value.rules?.map((rule) => (
            <span key={rule.id}>
              {rule.name} · {rule.required ? `${rule.required} required` : "Optional"}
              {rule.approved && " · Satisfied"}
            </span>
          ))}
          {value.warnings.map((warning) => (
            <p className="meta" key={warning}>
              {warning === "approvals_unavailable"
                ? "Approval state unavailable"
                : "Approval rules unavailable"}
            </p>
          ))}
        </>
      )}
    </div>
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
