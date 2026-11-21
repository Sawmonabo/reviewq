import { useEffect, useEffectEvent, useRef, useState } from "react";
import { historyDocument } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import { Loading, Notice } from "../../../components/PanelState";
import { useReadingInset } from "../../../hooks/useReadingInset";
import { useResource } from "../../../hooks/useResource";
import { documentDisplay, documentSource, documentTree, unavailableWindow } from "./document";

export function NativeDocument({
  target,
  entryId,
  generation,
  reference,
  fallback,
  onReadRange,
}: {
  readonly target: SessionAddress;
  readonly entryId: string;
  readonly generation: string;
  readonly reference: string;
  readonly fallback: React.ReactNode;
  readonly onReadRange?: ((element: HTMLElement) => boolean) | undefined;
}) {
  const [position, setPosition] = useState<{ byteOffset: number; sha256?: string }>({
    byteOffset: 0,
  });
  const [showSource, setShowSource] = useState(false);
  const [blocked, setBlocked] = useState(false);
  const [jump, setJump] = useState("");
  const output = useRef<HTMLDivElement>(null);
  const wanted = useRef(0);
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
        if (data.result.state === "ready" && data.byteLength <= 65_536 && data.nextByte !== null)
          return { data: null, cursor };
        const source = documentSource(data);
        const display = documentDisplay(data);
        const tree = documentTree(data, source);
        const { result, ...range } = data;
        return {
          data: {
            range,
            source,
            display,
            tree,
            sourceOnly: result.state === "source",
            table:
              result.state === "table"
                ? {
                    first: result.page.firstRow,
                    count: result.page.rowCount,
                    total: result.page.totalRows,
                  }
                : undefined,
          },
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
  const remember = useReadingInset(output, shown);
  const holdReading = useEffectEvent((element: HTMLElement | null, pagedTable: boolean) => {
    if (element && pagedTable && onReadRange?.(element)) return;
    remember(element);
  });
  useEffect(() => {
    const next = range.data;
    if (next === undefined || next === shown) return;
    const events = new AbortController();
    function commit() {
      if (
        events.signal.aborted ||
        wanted.current !== position.byteOffset ||
        selectionIn(output.current)
      )
        return;
      const active = document.activeElement;
      if (active && active !== output.current && output.current?.contains(active)) return;
      if (next) holdReading(output.current, next.table !== undefined);
      setShown(next);
    }
    document.addEventListener("selectionchange", commit, { signal: events.signal });
    document.addEventListener("focusin", commit, { signal: events.signal });
    document.addEventListener("focusout", () => queueMicrotask(commit), { signal: events.signal });
    commit();
    return () => events.abort();
  }, [position.byteOffset, range.data, shown]);
  if (shown === null) return fallback;
  const data = shown?.range;

  function preserveSelection() {
    const selected = selectionIn(output.current);
    setBlocked(selected);
    return !selected;
  }

  function move(offset: number) {
    if (!data || !preserveSelection()) return;
    wanted.current = offset;
    output.current?.focus({ preventScroll: true });
    setPosition({ byteOffset: offset, sha256: data.sha256 });
  }

  const navigation = data && (
    <div className="actions output-navigation" aria-busy={range.pending}>
      {shown?.table && (
        <span className="meta">
          Rows {shown.table.first + 1}–{shown.table.first + shown.table.count} of{" "}
          {shown.table.total}
        </span>
      )}
      {data.previousByte !== null && (
        <>
          <button type="button" className="quiet-button" onClick={() => move(0)}>
            Start of output
          </button>
          <button type="button" className="quiet-button" onClick={() => move(data.previousByte!)}>
            Earlier output
          </button>
        </>
      )}
      {data.tableStart != null &&
        data.tableStart >= data.byteEnd &&
        data.tableStart !== data.nextByte && (
          <button type="button" className="quiet-button" onClick={() => move(data.tableStart!)}>
            Read table rows
          </button>
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
            if (jump && Number.isSafeInteger(offset) && offset >= 0 && offset <= data.byteLength)
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
  );
  return (
    <div className="output-reader stack">
      {shown?.table && navigation}
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
            <MarkdownContent source={shown.source} formattingLimit />
          ) : (
            <pre className="tool-output-line" aria-label="Native output text">
              {shown.source}
            </pre>
          ))}
      </div>
      {!shown?.table && navigation}
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
