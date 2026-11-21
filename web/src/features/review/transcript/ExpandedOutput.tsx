import { useRef, useState } from "react";
import { historyContent } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import type { HistoryContent } from "../../../api/generated/contracts";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import type { FoundOutput } from "./find/destination";
import { NativeDocument } from "./NativeDocument";

type Position = { readonly offset: number; readonly cursor?: string };
const start: Position = { offset: 0 };
const encoder = new TextEncoder();
const MAX_POSITIONS = 32;

type Props = {
  readonly target: SessionAddress;
  readonly entryId: string;
  readonly generation: string;
  readonly reference: string;
  readonly format: "markdown" | "text";
  readonly found?: FoundOutput;
  readonly onReadRange?: ((element: HTMLElement) => boolean) | undefined;
};

export function ExpandedOutput(props: Props) {
  const source = <SourceOutput {...props} />;
  if (!props.found?.document && (props.format !== "markdown" || props.found?.content))
    return source;
  return (
    <NativeDocument
      key={props.reference}
      target={props.target}
      generation={props.generation}
      entryId={props.entryId}
      reference={props.reference}
      fallback={source}
      prepared={props.found?.document}
      onReadRange={props.onReadRange}
    />
  );
}

function SourceOutput({ target, entryId, generation, reference, format, found }: Props) {
  const opening = found?.destination;
  const [path, setPath] = useState<readonly Position[]>(() => [
    opening
      ? {
          offset: opening.offset,
          ...(opening.contentCursor ? { cursor: opening.contentCursor } : {}),
        }
      : start,
  ]);
  const [blocked, setBlocked] = useState(false);
  const output = useRef<HTMLDivElement>(null);
  const position = path.at(-1) ?? start;
  const prepared = opening?.offset === position.offset && found?.content ? found : undefined;
  const initialRange = prepared && outputRange(prepared.content);
  const range = useResource(
    `output:${target.runId}:${target.sessionKey}:${generation}:${reference}:${position.offset}`,
    async (signal) => {
      const data =
        prepared?.content ??
        (await historyContent(target, entryId, generation, reference, position.cursor, signal));
      const bytes = encoder.encode(data.text).length;
      if (data.offset !== position.offset || bytes > 65_536 || (!bytes && data.nextCursor)) {
        throw new Error("The requested native output range is unavailable.");
      }
      return {
        data: outputRange(data),
        cursor: "",
      };
    },
  );
  const content = initialRange ?? range.data;
  const next = content?.nextCursor;

  function move(next: readonly Position[]) {
    const selection = getSelection();
    if (
      output.current &&
      selection &&
      !selection.isCollapsed &&
      selection.rangeCount &&
      selection.getRangeAt(0).intersectsNode(output.current)
    ) {
      setBlocked(true);
      return;
    }
    setBlocked(false);
    output.current?.focus({ preventScroll: true });
    setPath(next);
  }

  return (
    <div className="output-reader stack">
      <div
        ref={output}
        tabIndex={-1}
        aria-label="Native output"
        data-find-body={prepared ? "" : undefined}
      >
        {!content && range.pending && <Loading label="Reading native output…" />}
        {range.error && <Notice retry={range.retry}>{range.error}</Notice>}
        {content &&
          (format === "markdown" && content.complete ? (
            prepared?.tree ? (
              <MarkdownContent tree={prepared.tree} display={content.display} />
            ) : (
              <MarkdownContent source={content.text} display={content.display} />
            )
          ) : (
            <pre className="tool-output-line" aria-label="Native output text">
              {content.text}
            </pre>
          ))}
      </div>
      {(position.offset > 0 || next) && (
        <div className="actions output-navigation">
          {position.offset > 0 && (
            <button type="button" className="quiet-button" onClick={() => move([start])}>
              Start of output
            </button>
          )}
          {path.length > 1 && (
            <button type="button" className="quiet-button" onClick={() => move(path.slice(0, -1))}>
              Earlier output
            </button>
          )}
          {next && content && (
            <button
              type="button"
              className="quiet-button"
              onClick={() =>
                move([...path, { offset: content.nextOffset, cursor: next }].slice(-MAX_POSITIONS))
              }
            >
              More output
            </button>
          )}
        </div>
      )}
      {blocked && (
        <Notice>
          Clear the active text selection to change output ranges.{" "}
          <button
            type="button"
            onClick={() => {
              getSelection()?.removeAllRanges();
              setBlocked(false);
            }}
          >
            Clear selection
          </button>
        </Notice>
      )}
    </div>
  );
}

function outputRange(data: HistoryContent) {
  return {
    text: data.text,
    display: data.display,
    complete: data.offset === 0 && data.complete && !data.nextCursor,
    nextCursor: data.nextCursor,
    nextOffset: data.offset + encoder.encode(data.text).length,
  };
}
