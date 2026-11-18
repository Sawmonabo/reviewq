import { useCallback, useEffect, useState } from "react";
import { ApiResponseError } from "../../../api/client";
import {
  downloadPatchUrl,
  isRichPatch,
  PATCH_WINDOW_BYTES,
  readPatch,
  readPatchDiscussions,
  readPatchFiles,
} from "../../../api/changes";
import type { PatchWindow } from "../../../api/changes";
import type {
  PatchDiscussion,
  PatchDiscussions,
  PatchFile,
  PatchFileIndex,
} from "../../../api/generated/contracts";
import { PatchDocumentView, DiscussionCard } from "./PatchDocumentView";
import "./changes.css";

type Bookmark = { readonly file: string; readonly revision: string; readonly scroll: number };
const bookmarks = new Map<string, Bookmark>();

export function Changes({ reviewId }: { readonly reviewId: string }) {
  const [generation, setGeneration] = useState(0);
  const [index, setIndex] = useState<PatchFileIndex>();
  const [fileId, setFileId] = useState<string>();
  const [error, setError] = useState<string>();
  const [cursor, setCursor] = useState<{ revision: string; cursor: string }>();
  const [canBegin, setCanBegin] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    const saved = cursor === undefined && generation === 0 ? readBookmark(reviewId) : undefined;
    const preferred = saved?.file;
    async function fetchIndex() {
      let result: PatchFileIndex;
      try {
        result = await readPatchFiles(reviewId, controller.signal, cursor, preferred);
      } catch (reason) {
        if (!preferred || controller.signal.aborted || !(reason instanceof ApiResponseError))
          throw reason;
        result = await readPatchFiles(reviewId, controller.signal, cursor);
      }
      if (controller.signal.aborted) return;
      const fallback =
        result.items.find((row) => row.availability === "complete") ?? result.items[0];
      setIndex(result);
      setFileId(result.items.find((row) => row.fileId === preferred)?.fileId ?? fallback?.fileId);
      setError(undefined);
      setCanBegin(cursor !== undefined);
    }
    void fetchIndex().catch((reason: unknown) => {
      if (!controller.signal.aborted) setError(errorMessage(reason));
    });
    return () => controller.abort();
  }, [reviewId, generation, cursor]);

  const selected = index?.items.find((file) => file.fileId === fileId);

  function reload() {
    setIndex(undefined);
    setFileId(undefined);
    setError(undefined);
    setCursor(undefined);
    setGeneration((value) => value + 1);
  }

  return (
    <section className="changes" aria-label="Changes">
      {error && (
        <div role="status" className="changes-notice">
          {error} <button onClick={reload}>Reload file list</button>
        </div>
      )}
      {!error && !index && <p aria-busy="true">Loading changed files…</p>}
      {index && (
        <>
          {index.items.length === 0 && <p>No changed files were returned.</p>}
          {index.items.length !== 0 && (
            <div className="changes-file-toolbar">
              <label>
                File{" "}
                <select value={fileId} onChange={(event) => setFileId(event.currentTarget.value)}>
                  {index.items.map((file) => (
                    <option key={file.fileId} value={file.fileId}>
                      {file.renamed ? `${file.oldPath} → ${file.newPath}` : file.newPath}
                    </option>
                  ))}
                </select>
              </label>
              {canBegin && <button onClick={() => setCursor(undefined)}>First files</button>}
              {index.nextCursor && (
                <button
                  onClick={() => {
                    const next = index.nextCursor;
                    if (next) setCursor({ revision: index.revision, cursor: next });
                  }}
                >
                  More files
                </button>
              )}
            </div>
          )}
          {selected && (
            <SelectedPatch
              key={JSON.stringify([selected.reviewId, selected.revision, selected.fileId])}
              file={selected}
              onReload={reload}
            />
          )}
        </>
      )}
    </section>
  );
}

function SelectedPatch({
  file,
  onReload,
}: {
  readonly file: PatchFile;
  readonly onReload: () => void;
}) {
  const rich = isRichPatch(file);
  const [document, setDocument] = useState<PatchWindow>();
  const [error, setError] = useState<string>();
  const [offset, setOffset] = useState(0);
  const [retry, setRetry] = useState(0);
  const [previous, setPrevious] = useState<readonly number[]>([]);
  const [etag, setEtag] = useState<string>();
  const [stale, setStale] = useState(false);
  const [discussions, setDiscussions] = useState<PatchDiscussions>();
  const [threadsError, setThreadsError] = useState<string>();
  const [moreThreads, setMoreThreads] = useState(false);

  useEffect(() => {
    if (file.availability !== "complete") return;
    const cancel = new AbortController();
    void readPatch(
      file,
      cancel.signal,
      rich ? undefined : { offset, ...(etag ? { etag } : {}) },
    ).then(
      (data) => {
        if (!cancel.signal.aborted) {
          setDocument(data);
          setError(undefined);
        }
      },
      (reason: unknown) => {
        if (cancel.signal.aborted) return;
        if (scopeChanged(reason)) setStale(true);
        else setError(errorMessage(reason));
      },
    );
    return () => cancel.abort();
  }, [file, rich, offset, etag, retry]);

  useEffect(() => {
    const cancel = new AbortController();
    let timer: number | undefined;
    async function observe() {
      try {
        const latest = await readPatchDiscussions(file.reviewId, file.revision, cancel.signal);
        if (!cancel.signal.aborted) {
          setDiscussions(latest);
          setThreadsError(undefined);
        }
      } catch (reason) {
        if (cancel.signal.aborted) return;
        if (scopeChanged(reason)) {
          setStale(true);
          return;
        }
        setThreadsError(errorMessage(reason));
      }
      if (!cancel.signal.aborted) timer = window.setTimeout(() => void observe(), 1500);
    }
    if (!moreThreads) void observe();
    return () => {
      cancel.abort();
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [file.reviewId, file.revision, moreThreads]);

  const restoreScroll = useCallback(() => {
    const saved = readBookmark(file.reviewId);
    return saved?.file === file.fileId && saved.revision === file.revision ? saved.scroll : 0;
  }, [file]);

  const rememberScroll = useCallback(
    (scroll: number) => {
      saveBookmark(file.reviewId, { file: file.fileId, revision: file.revision, scroll });
    },
    [file],
  );

  function pageTo(next: number) {
    if (document && !etag) setEtag(document.etag);
    setOffset(next);
  }

  async function readMoreThreads() {
    const current = discussions;
    if (!current?.nextCursor) return;
    setMoreThreads(true);
    try {
      // One additional metadata page is shown at a time.
      const page = await readPatchDiscussions(
        file.reviewId,
        file.revision,
        AbortSignal.timeout(10_000),
        current.nextCursor,
      );
      setDiscussions(page);
    } catch (reason) {
      if (scopeChanged(reason)) setStale(true);
      else setThreadsError(errorMessage(reason));
    }
  }

  const active = (discussions?.items ?? []).filter((thread) => matchesFile(thread, file));
  const separate = (discussions?.items ?? []).filter(
    (thread) => !rich || !matchesFile(thread, file),
  );
  return (
    <>
      {stale && (
        <p role="status" className="changes-notice">
          The source revision changed. <button onClick={onReload}>Load current changes</button>
        </p>
      )}
      {file.availability !== "complete" ? (
        <p className="changes-notice">{availability(file.availability)}</p>
      ) : (
        <>
          <div className="patch-toolbar">
            <a href={downloadPatchUrl(file)}>Download patch</a>
            {!rich && document && (
              <div aria-label="Patch byte ranges" className="range-toolbar">
                <span>Large patch · source view</span>
                <span>
                  Bytes {document.first + 1}–{document.next} of {document.total}
                </span>
                <button disabled={offset === 0} onClick={() => pageTo(0)}>
                  Start
                </button>
                <button
                  disabled={offset === 0}
                  onClick={() => {
                    const prior = previous.at(-1);
                    pageTo(prior ?? Math.max(0, offset - PATCH_WINDOW_BYTES));
                    setPrevious(previous.slice(0, -1));
                  }}
                >
                  Previous
                </button>
                <button
                  disabled={document.next === document.total}
                  onClick={() => {
                    setPrevious([...previous, offset]);
                    pageTo(document.next);
                  }}
                >
                  Next
                </button>
                <button
                  disabled={document.next === document.total}
                  onClick={() => {
                    setPrevious([...previous, offset]);
                    pageTo(Math.max(0, document.total - PATCH_WINDOW_BYTES));
                  }}
                >
                  End
                </button>
                <span>Find covers the displayed range.</span>
              </div>
            )}
          </div>
          {error && (
            <p role="status">
              {error} <button onClick={() => setRetry(retry + 1)}>Retry</button>
            </p>
          )}
          {!document && !stale && !error && <p aria-busy="true">Loading patch…</p>}
          {document?.text === "" && <p>This file has an empty patch.</p>}
          {document &&
            document.text !== "" &&
            (rich ? (
              <PatchDocumentView
                text={document.text}
                discussions={active}
                restoreScroll={restoreScroll}
                rememberScroll={rememberScroll}
              />
            ) : (
              <pre className="patch-source" tabIndex={0} aria-label="Patch source window">
                {document.text}
              </pre>
            ))}
        </>
      )}
      <section className="discussion-list" aria-label="Review discussions">
        <h3>Discussions</h3>
        {threadsError && <p role="status">{threadsError}</p>}
        {rich && active.length > 0 && <p>{active.length} on this file</p>}
        {separate.map((thread) => (
          <DiscussionCard key={thread.discussionId} thread={thread} />
        ))}
        {discussions?.items.length === 0 && <p>No discussions on this revision.</p>}
        {discussions?.nextCursor && (
          <button onClick={() => void readMoreThreads()}>More discussions</button>
        )}
      </section>
    </>
  );
}

function matchesFile(thread: PatchDiscussion, file: PatchFile): boolean {
  return (
    thread.anchor !== null &&
    thread.anchor.oldPath === file.oldPath &&
    thread.anchor.newPath === file.newPath &&
    (thread.anchor.newLine !== null || thread.anchor.oldLine !== null)
  );
}

function scopeChanged(reason: unknown): boolean {
  return (
    reason instanceof ApiResponseError &&
    (reason.code === "stale_revision" || reason.code === "precondition_failed")
  );
}

function availability(state: PatchFile["availability"]): string {
  switch (state) {
    case "binary":
      return "A text diff is unavailable for this binary file.";
    case "provider_limited":
      return "GitLab omitted the patch for this file.";
    case "incomplete":
      return "GitLab returned incomplete patch coverage.";
    default:
      return "The selected patch is unavailable.";
  }
}

function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : "ReviewQ could not load these changes.";
}

function readBookmark(reviewId: string): Bookmark | undefined {
  const current = bookmarks.get(reviewId);
  if (current) return current;
  try {
    const raw: unknown = JSON.parse(sessionStorage.getItem(`reviewq.patch.${reviewId}`) ?? "null");
    if (
      typeof raw === "object" &&
      raw !== null &&
      "file" in raw &&
      "revision" in raw &&
      "scroll" in raw &&
      typeof raw.file === "string" &&
      typeof raw.revision === "string" &&
      typeof raw.scroll === "number" &&
      Number.isFinite(raw.scroll) &&
      raw.scroll >= 0
    ) {
      return { file: raw.file, revision: raw.revision, scroll: raw.scroll };
    }
  } catch {
    // In-memory bookmarks remain available when browser storage is disabled.
  }
  return undefined;
}

function saveBookmark(reviewId: string, bookmark: Bookmark) {
  if (!bookmarks.has(reviewId) && bookmarks.size >= 128) {
    const oldest = bookmarks.keys().next().value;
    if (oldest !== undefined) {
      bookmarks.delete(oldest);
      try {
        sessionStorage.removeItem(`reviewq.patch.${oldest}`);
      } catch {
        /* storage is optional */
      }
    }
  }
  bookmarks.set(reviewId, bookmark);
  try {
    sessionStorage.setItem(`reviewq.patch.${reviewId}`, JSON.stringify(bookmark));
  } catch {
    /* storage is optional */
  }
}
