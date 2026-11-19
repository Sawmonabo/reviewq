import { useRef, useState } from "react";
import { historyContent } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { TranscriptMarkdown } from "./TranscriptMarkdown";

type Position = { readonly offset: number; readonly cursor?: string };
const start: Position = { offset: 0 };
const encoder = new TextEncoder();
const MAX_POSITIONS = 32;

export function ExpandedOutput({
  target,
  entryId,
  generation,
  reference,
  format,
}: {
  readonly target: SessionAddress;
  readonly entryId: string;
  readonly generation: string;
  readonly reference: string;
  readonly format: "markdown" | "text";
}) {
  const [path, setPath] = useState<readonly Position[]>([start]);
  const [blocked, setBlocked] = useState(false);
  const output = useRef<HTMLDivElement>(null);
  const position = path.at(-1) ?? start;
  const range = useResource(
    `output:${target.runId}:${target.sessionKey}:${generation}:${reference}:${position.offset}`,
    async (signal) => {
      const data = await historyContent(
        target,
        entryId,
        generation,
        reference,
        position.cursor,
        signal,
      );
      const bytes = encoder.encode(data.text).length;
      if (data.offset !== position.offset || bytes > 65_536 || (!bytes && data.nextCursor)) {
        throw new Error("The requested native output range is unavailable.");
      }
      return {
        data: { text: data.text, nextCursor: data.nextCursor, nextOffset: data.offset + bytes },
        cursor: "",
      };
    },
  );
  const next = range.data?.nextCursor;

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
      <div ref={output} tabIndex={-1} aria-label="Native output">
        {range.pending && <Loading label="Reading native output…" />}
        {range.error && <Notice retry={range.retry}>{range.error}</Notice>}
        {range.data &&
          (format === "markdown" ? (
            <TranscriptMarkdown source={range.data.text} />
          ) : (
            <pre className="tool-output-line" aria-label="Native output text">
              {range.data.text}
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
          {next && range.data && (
            <button
              type="button"
              className="quiet-button"
              onClick={() => {
                if (range.data)
                  move(
                    [...path, { offset: range.data.nextOffset, cursor: next }].slice(
                      -MAX_POSITIONS,
                    ),
                  );
              }}
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
