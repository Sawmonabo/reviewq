import { memo, useState } from "react";
import { ResponseSizeLimit } from "../../../api/bounded-bytes";
import {
  downloadPatchUrl,
  isRichPatch,
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
import { PatchSource } from "./PatchSource";
import type { PatchLine, SourcePosition } from "./patch-position";

export const FilePatch = memo(function FilePatch({
  file,
  threads,
  source,
  position,
  admitted,
  visible,
  priorityLine,
  sourcePosition,
  enterFromEnd,
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly source: string | undefined;
  readonly position: number;
  readonly admitted: boolean;
  readonly visible: boolean;
  readonly priorityLine: PatchLine | undefined;
  readonly sourcePosition: SourcePosition | "start" | undefined;
  readonly enterFromEnd: boolean;
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
          sourcePosition={sourcePosition}
          enterFromEnd={enterFromEnd}
        />
      ) : (
        <div className="patch-waiting" aria-hidden="true" />
      )}
      {file.availability !== "complete" &&
        threads.map((thread) => <DiscussionThread key={thread.discussionId} thread={thread} />)}
    </section>
  );
});

type ContentsProps = {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly visible: boolean;
  readonly priorityLine: PatchLine | undefined;
  readonly sourcePosition: SourcePosition | "start" | undefined;
  readonly enterFromEnd: boolean;
};

function PatchContents(props: ContentsProps) {
  const [overLimit, setOverLimit] = useState(false);
  if (overLimit || !isRichPatch(props.file))
    return (
      <>
        <PatchSource
          file={props.file}
          position={props.sourcePosition}
          enterFromEnd={props.enterFromEnd}
        />
        {props.threads.map((thread) => (
          <DiscussionThread key={thread.discussionId} thread={thread} />
        ))}
      </>
    );
  return <RichPatch {...props} onLimit={() => setOverLimit(true)} />;
}

function RichPatch({
  file,
  threads,
  visible,
  priorityLine,
  onLimit,
}: ContentsProps & { readonly onLimit: () => void }) {
  const [seen, setSeen] = useState(visible);
  if (visible && !seen) setSeen(true);
  const patch = useResource(JSON.stringify([file.revision, file.fileId]), async (signal) => {
    try {
      return { cursor: "", data: await readPatch(file, signal) };
    } catch (error) {
      if (
        !signal.aborted &&
        (error instanceof PatchFormattingLimit || error instanceof ResponseSizeLimit)
      )
        onLimit();
      throw error;
    }
  });
  const body = patch.data;
  const ready = Boolean(body && (seen || (file.lineCount ?? 0) <= 128));
  return (
    <>
      {patch.error && <Notice retry={patch.retry}>{patch.error}</Notice>}
      {(patch.pending || (body && !ready)) && (
        <div className="patch-waiting">
          {patch.pending && visible && <Loading label="Loading patch…" />}
        </div>
      )}
      {body?.text === "" && <Empty title="This file has an empty patch" />}
      {body && ready && (
        <PatchDocumentView text={body.text} discussions={threads} priorityLine={priorityLine} />
      )}
      {!body &&
        patch.error &&
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
