import { useEffect, useRef, useState } from "react";
import { historyDocument } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { documentDisplay, documentSource, documentTree, unavailableWindow } from "./document";

export function NativeDocument({
  target,
  entryId,
  generation,
  reference,
  fallback,
}: {
  readonly target: SessionAddress;
  readonly entryId: string;
  readonly generation: string;
  readonly reference: string;
  readonly fallback: React.ReactNode;
}) {
  const [position, setPosition] = useState<{ byteOffset: number; sha256?: string }>({
    byteOffset: 0,
  });
  const [showSource, setShowSource] = useState(false);
  const [blocked, setBlocked] = useState(false);
  const [jump, setJump] = useState("");
  const output = useRef<HTMLDivElement>(null);
  const range = useResource(
    `document:${target.runId}:${target.sessionKey}:${generation}:${reference}:${position.byteOffset}`,
    async (signal) => {
      try {
        const { data, cursor } = await historyDocument(
          target,
          entryId,
          generation,
          reference,
          position,
          signal,
        );
        if (data.byteLength <= 65_536 && data.nextByte !== null) return { data: null, cursor };
        const source = documentSource(data);
        const display = documentDisplay(data);
        const tree = documentTree(data, source);
        const { result, ...range } = data;
        return {
          data: { range, source, display, tree, sourceOnly: result.state === "source" },
          cursor,
        };
      } catch (error) {
        signal.throwIfAborted();
        if (position.sha256 === undefined && unavailableWindow(error))
          return { data: null, cursor: "" };
        throw error;
      }
    },
  );
  const [shown, setShown] = useState(range.data);
  useEffect(() => {
    const next = range.data;
    if (next === undefined) return;
    const events = new AbortController();
    function commit() {
      if (selectionIn(output.current)) return;
      const active = document.activeElement;
      if (active && active !== output.current && output.current?.contains(active)) return;
      setShown(next);
    }
    document.addEventListener("selectionchange", commit, { signal: events.signal });
    document.addEventListener("focusin", commit, { signal: events.signal });
    commit();
    return () => events.abort();
  }, [range.data]);
  if (shown === null) return fallback;
  const data = shown?.range;

  function preserveSelection() {
    const selected = selectionIn(output.current);
    setBlocked(selected);
    return !selected;
  }

  function move(offset: number) {
    if (!data || !preserveSelection()) return;
    output.current?.focus({ preventScroll: true });
    setPosition({ byteOffset: offset, sha256: data.sha256 });
  }

  return (
    <div className="output-reader stack">
      <div ref={output} tabIndex={-1} aria-label="Native output">
        {!shown && range.pending && <Loading label="Reading native output…" />}
        {range.error && <Notice retry={range.retry}>{range.error}</Notice>}
        {shown &&
          (!showSource && shown.tree ? (
            <MarkdownContent tree={shown.tree} display={shown.display} />
          ) : !showSource &&
            shown.sourceOnly &&
            data &&
            data.byteStart === 0 &&
            data.byteEnd === data.byteLength ? (
            <MarkdownContent source={shown.source} />
          ) : (
            <pre className="tool-output-line" aria-label="Native output text">
              {shown.source}
            </pre>
          ))}
      </div>
      {data && (
        <div className="actions output-navigation">
          {data.previousByte !== null && (
            <>
              <button type="button" className="quiet-button" onClick={() => move(0)}>
                Start of output
              </button>
              <button
                type="button"
                className="quiet-button"
                onClick={() => move(data.previousByte!)}
              >
                Earlier output
              </button>
            </>
          )}
          {data.nextByte !== null && (
            <button type="button" className="quiet-button" onClick={() => move(data.nextByte!)}>
              More output
            </button>
          )}
          {shown?.tree && (
            <button
              type="button"
              className="quiet-button"
              aria-pressed={showSource}
              onClick={() => {
                if (preserveSelection()) setShowSource(!showSource);
              }}
            >
              {showSource ? "Read formatted output" : "Read source"}
            </button>
          )}
          {(data.previousByte !== null || data.nextByte !== null) && (
            <form
              className="actions"
              onSubmit={(event) => {
                event.preventDefault();
                const offset = Number(jump);
                if (
                  jump &&
                  Number.isSafeInteger(offset) &&
                  offset >= 0 &&
                  offset <= data.byteLength
                )
                  move(offset);
              }}
            >
              <label>
                Source byte
                <input
                  type="number"
                  min={0}
                  max={data.byteLength}
                  value={jump}
                  onChange={(event) => setJump(event.currentTarget.value)}
                />
              </label>
              <button type="submit" className="quiet-button">
                Go
              </button>
            </form>
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

function selectionIn(output: HTMLElement | null) {
  const selected = getSelection();
  return Boolean(
    output &&
    selected &&
    !selected.isCollapsed &&
    selected.rangeCount &&
    selected.getRangeAt(0).intersectsNode(output),
  );
}
