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
import { DiffCount, FileKind } from "./ChangedFilePicker";
import { ChangesThread } from "./ChangesThread";
import { splitPath } from "./file-search";
import { PatchDocumentView } from "./PatchDocumentView";
import { PatchSource } from "./PatchSource";
import type { PatchLine, SourcePosition } from "./patch-position";
import "./patch.css";

export const FilePatch = memo(function FilePatch({
  file,
  threads,
  source,
  position,
  admitted,
  visible,
  sourcePosition,
  sourceLine,
  entrySide,
  onSourceChanged,
}: {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly source: string | undefined;
  readonly position: number;
  readonly admitted: boolean;
  readonly visible: boolean;
  readonly sourcePosition: SourcePosition | "start" | "away" | undefined;
  readonly sourceLine: PatchLine | undefined;
  readonly entrySide: "start" | "end" | undefined;
  readonly onSourceChanged: () => void;
}) {
  return (
    <section
      className="selected-patch"
      aria-label={`Patch for ${file.newPath}`}
      data-file-id={file.fileId}
      data-file-position={position}
    >
      <header className="patch-toolbar">
        <div className="patch-file-title" tabIndex={-1}>
          <h3>
            <code>
              {file.renamed && (
                <>
                  <PatchPathText path={file.oldPath} /> →{" "}
                </>
              )}
              <PatchPathText path={file.newPath} />
            </code>
          </h3>
          <span className="patch-file-kind">
            <FileKind file={file} />
          </span>
        </div>
        <div className="patch-links" hidden={file.availability !== "complete" && !source}>
          {file.availability === "complete" && (
            <a
              href={downloadPatchUrl(file)}
              aria-label="Download full patch source"
              title="Download full patch source"
            >
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
        <DiffCount file={file} />
      </header>
      {file.availability !== "complete" ? (
        <Notice>{availability(file.availability)}</Notice>
      ) : admitted ? (
        <PatchContents
          file={file}
          threads={threads}
          visible={visible}
          sourcePosition={sourcePosition}
          sourceLine={sourceLine}
          entrySide={entrySide}
          onSourceChanged={onSourceChanged}
        />
      ) : (
        <div className="patch-waiting" aria-hidden="true" />
      )}
      {file.availability !== "complete" &&
        threads.map((thread) => (
          <ChangesThread key={thread.discussionId} thread={thread} outside />
        ))}
    </section>
  );
});

function PatchPathText({ path }: { readonly path: string }) {
  const { cut, name } = splitPath(path);
  return (
    <>
      {cut > 0 && <span className="patch-folder">{path.slice(0, cut)}</span>}
      {name}
    </>
  );
}

type ContentsProps = {
  readonly file: PatchFile;
  readonly threads: readonly PatchDiscussion[];
  readonly visible: boolean;
  readonly sourcePosition: SourcePosition | "start" | "away" | undefined;
  readonly sourceLine: PatchLine | undefined;
  readonly entrySide: "start" | "end" | undefined;
  readonly onSourceChanged: () => void;
};

function PatchContents(props: ContentsProps) {
  const [overLimit, setOverLimit] = useState(false);
  if (overLimit || !isRichPatch(props.file))
    return (
      <>
        <PatchSource
          file={props.file}
          threads={props.threads}
          position={props.sourcePosition}
          line={props.sourceLine}
          entrySide={props.entrySide}
          onSourceChanged={props.onSourceChanged}
        />
        {!props.file.patchETag &&
          props.threads.map((thread) => (
            <ChangesThread key={thread.discussionId} thread={thread} outside />
          ))}
      </>
    );
  return <RichPatch {...props} onLimit={() => setOverLimit(true)} />;
}

function RichPatch({
  file,
  threads,
  visible,
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
      {body && ready && <PatchDocumentView file={file} text={body.text} discussions={threads} />}
      {!body &&
        patch.error &&
        threads.map((thread) => (
          <ChangesThread key={thread.discussionId} thread={thread} outside />
        ))}
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
