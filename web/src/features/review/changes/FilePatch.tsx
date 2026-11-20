import { memo, useState } from "react";
import { ResponseSizeLimit } from "../../../api/bounded-bytes";
import {
  downloadPatchUrl,
  isRichPatch,
  PATCH_WINDOW_BYTES,
  PatchFormattingLimit,
  readPatch,
} from "../../../api/changes";
import type { PatchDiscussion, PatchFile } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { DiscussionThread } from "../DiscussionThread";
import { DiffCount, FileKind } from "./ChangedFilePicker";
import { PatchDocumentView } from "./PatchDocumentView";
import type { PatchLine } from "./patch-position";

type Window = { readonly offset: number; readonly etag?: string; readonly last?: boolean };

export const FilePatch = memo(function FilePatch({
  file,
  threads,
  source,
  position,
  admitted,
  visible,
  priorityLine,
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly source: string | undefined;
  readonly position: number;
  readonly admitted: boolean;
  readonly visible: boolean;
  readonly priorityLine: PatchLine | undefined;
}) {
  return (
    <section
      className="selected-patch"
      aria-label={`Patch for ${file.newPath}`}
      data-file-id={file.fileId}
      data-file-position={position}
    >
      <div className="spread patch-toolbar">
        <div className="patch-file-title" tabIndex={-1}>
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
      </div>
      {file.availability !== "complete" ? (
        <Notice>{availability(file.availability)}</Notice>
      ) : admitted ? (
        <PatchContents
          file={file}
          threads={threads}
          visible={visible}
          priorityLine={priorityLine}
        />
      ) : (
        <div className="patch-waiting" aria-hidden="true" />
      )}
      {file.availability !== "complete" &&
        threads.map((thread) => <DiscussionThread key={thread.discussionId} thread={thread} />)}
    </section>
  );
});

function PatchContents({
  file,
  threads,
  visible,
  priorityLine,
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly visible: boolean;
  readonly priorityLine: PatchLine | undefined;
}) {
  const [sourceMode, setSourceMode] = useState(!isRichPatch(file));
  const [window, setWindow] = useState<Window>({ offset: 0 });
  const [back, setBack] = useState<readonly Window[]>([]);
  const [seen, setSeen] = useState(visible);
  if (visible && !seen) setSeen(true);
  const patch = useResource(
    JSON.stringify([file.revision, file.fileId, sourceMode, window]),
    async (signal) => {
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
  const ready = Boolean(body && (seen || (file.lineCount ?? 0) <= 128));

  function navigate(target: Window) {
    setBack((items) => [...items, window].slice(-12));
    setWindow(target);
  }

  return (
    <>
      {patch.error && <Notice retry={patch.retry}>{patch.error}</Notice>}
      {(patch.pending || (body && !ready)) && (
        <div className="patch-waiting">
          {patch.pending && visible && <Loading label="Loading patch…" />}
        </div>
      )}
      {body?.text === "" && <Empty title="This file has an empty patch" />}
      {body &&
        ready &&
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
          <PatchDocumentView text={body.text} discussions={threads} priorityLine={priorityLine} />
        ))}
      {(sourceMode || (!body && patch.error)) &&
        threads.map((thread) => <DiscussionThread key={thread.discussionId} thread={thread} />)}
    </>
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
