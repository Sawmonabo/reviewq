import { lazy, Suspense, useEffect, useRef, useState } from "react";
import type { HistoryContent, HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import { historyContent } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Notice } from "../../../components/PanelState";
import { describeError } from "../../../lib/presentation";
import { ToolHeading } from "./ToolActivity";
import { groupParts, openKey } from "./tool-activity";
import type { ToolCatalog } from "./tool-activity";

const Markdown = lazy(() =>
  import("../../../components/markdown/MarkdownContent").then((module) => ({
    default: module.MarkdownContent,
  })),
);
const LIMIT = 4;
const encoder = new TextEncoder();

type Props = {
  readonly target: SessionAddress;
  readonly entry: HistoryEntry;
  readonly knownTools: ToolCatalog;
  readonly generation: string;
  readonly reference: string | undefined;
  readonly onReference: (reference: string | undefined) => void;
  readonly expanded: ReadonlySet<string>;
  readonly toggle: (key: string, open: boolean) => void;
  readonly extra?: { readonly items: readonly HistoryPart[]; readonly next: string | null };
  readonly onMoreParts: (cursor: string) => void;
};

export function EntryBody({
  entry,
  knownTools,
  target,
  generation,
  reference,
  onReference,
  expanded,
  toggle,
  extra,
  onMoreParts,
}: Props) {
  const next = extra ? extra.next : entry.partsCursor;
  const parts = groupParts(entry, [...entry.parts, ...(extra?.items ?? [])], knownTools);

  function value(part: HistoryPart, key: string) {
    return (
      <div className="stack" key={key}>
        {part.kind === "assistant_text" ||
        part.kind === "user_prompt" ||
        part.kind === "assignment" ||
        part.kind === "plan" ? (
          <TranscriptMarkdown source={part.text} />
        ) : (
          <pre className="tool-output-line">{part.text}</pre>
        )}
        {part.contentRef && (
          <button
            type="button"
            onClick={() =>
              onReference(
                reference === part.contentRef ? undefined : (part.contentRef ?? undefined),
              )
            }
            aria-expanded={reference === part.contentRef}
          >
            {reference === part.contentRef ? "Close full output" : "Read full output"}
          </button>
        )}
      </div>
    );
  }

  return (
    <>
      {parts.map((group, index) => {
        if (group.kind === "prose") return value(group.part, `part:${index}`);
        const key = openKey(entry.entryId, group.callId);
        const open = expanded.has(key);
        return (
          <details
            className="tool-disclosure"
            key={`${key}:${index}`}
            open={open}
            onToggle={(event) => toggle(key, event.currentTarget.open)}
          >
            <summary data-tool-call={group.callId}>
              <ToolHeading call={group.call} state={group.state} timestamp={entry.timestamp} />
            </summary>
            {open && (
              <div className="tool-content stack">
                {group.parts.map((item, ordinal) => value(item, `tool:${ordinal}`))}
              </div>
            )}
          </details>
        );
      })}
      {entry.contentRef && (
        <button
          type="button"
          onClick={() => onReference(reference === entry.contentRef ? undefined : entry.contentRef)}
          aria-expanded={reference === entry.contentRef}
        >
          {reference === entry.contentRef ? "Close complete entry" : "Read complete entry"}
        </button>
      )}
      {next && (
        <button type="button" onClick={() => onMoreParts(next)}>
          More parts
        </button>
      )}
      {reference && (
        <ExpandedOutput
          key={reference}
          target={target}
          generation={generation}
          entryId={entry.entryId}
          reference={reference}
        />
      )}
    </>
  );
}

export function TranscriptMarkdown({ source }: { readonly source: string }) {
  return (
    <Suspense fallback={<p className="message-copy">{source}</p>}>
      <Markdown source={source} />
    </Suspense>
  );
}

function ExpandedOutput({
  target,
  entryId,
  generation,
  reference,
}: {
  readonly target: SessionAddress;
  readonly entryId: string;
  readonly generation: string;
  readonly reference: string;
}) {
  const [ranges, setRanges] = useState<readonly HistoryContent[]>([]);
  const [error, setError] = useState<string>();
  const [waiting, setWaiting] = useState(false);
  const request = useRef<AbortController | undefined>(undefined);
  const text = useRef<HTMLPreElement>(null);
  const latest = ranges.at(-1);
  useEffect(() => () => request.current?.abort(), []);

  async function read(cursor?: string, replace = false) {
    if (request.current) return;
    const selection = getSelection();
    if (
      (ranges.length >= LIMIT || replace) &&
      text.current &&
      selection &&
      !selection.isCollapsed &&
      selection.rangeCount &&
      selection.getRangeAt(0).intersectsNode(text.current)
    ) {
      setWaiting(true);
      return;
    }
    const controller = new AbortController();
    request.current = controller;
    setError(undefined);
    try {
      const range = await historyContent(
        target,
        entryId,
        generation,
        reference,
        cursor,
        controller.signal,
      );
      if (controller.signal.aborted) return;
      const expectedOffset = latest ? latest.offset + encoder.encode(latest.text).length : 0;
      if (
        (!replace && range.offset !== expectedOffset) ||
        encoder.encode(range.text).length > 65_536
      )
        throw new Error("The next native output range is unavailable.");
      setRanges((items) => (replace ? [range] : [...items, range].slice(-LIMIT)));
      setWaiting(false);
    } catch (failure) {
      if (!controller.signal.aborted) setError(describeError(failure));
    } finally {
      if (request.current === controller) request.current = undefined;
    }
  }

  return (
    <div className="output-reader surface-card stack">
      {!ranges.length && (
        <button type="button" onClick={() => void read()}>
          Load native output
        </button>
      )}
      {error && <Notice>{error}</Notice>}
      {ranges.length > 0 && (
        <pre ref={text} className="tool-output-line" aria-label="Native output text">
          {ranges.map((range) => (
            <span key={range.offset}>{range.text}</span>
          ))}
        </pre>
      )}
      <div className="actions">
        {(ranges[0]?.offset ?? 0) > 0 && (
          <button type="button" onClick={() => void read(undefined, true)}>
            Start of output
          </button>
        )}
        {latest?.nextCursor && (
          <button type="button" onClick={() => void read(latest.nextCursor ?? undefined)}>
            More output
          </button>
        )}
      </div>
      {waiting && (
        <Notice>
          More output available.{" "}
          <button
            type="button"
            onClick={() => {
              getSelection()?.removeAllRanges();
              setWaiting(false);
            }}
          >
            Clear selection
          </button>
        </Notice>
      )}
    </div>
  );
}
