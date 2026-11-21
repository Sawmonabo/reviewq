import { useState } from "react";
import type { DocumentDisplay } from "../../../api/generated/contracts/common";
import type { HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import type { SessionAddress } from "../../../api/history";
import { ExpandedOutput } from "./ExpandedOutput";
import { ToolHeading } from "./ToolActivity";
import { groupParts, openKey } from "./tool-activity";
import type { ToolCatalog } from "./tool-activity";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import type { ProvisionalReply } from "./timeline-rows";
import { inlineProseReference } from "./useInlineProse";
import type { InlineProse } from "./useInlineProse";
import type { FoundOutput } from "./find/destination";

type Props = {
  readonly target: SessionAddress;
  readonly entry: HistoryEntry | undefined;
  readonly provisional: ProvisionalReply | undefined;
  readonly knownTools: ToolCatalog;
  readonly generation: string;
  readonly reference: string | undefined;
  readonly onReference: (reference: string | undefined) => void;
  readonly expanded: ReadonlySet<string>;
  readonly prose: ReadonlyMap<string, InlineProse | null>;
  readonly toggle: (key: string, open: boolean) => void;
  readonly extra?: { readonly items: readonly HistoryPart[]; readonly next: string | null };
  readonly onMoreParts: (cursor: string) => void;
  readonly found?: FoundOutput;
};

export function EntryBody({
  entry,
  provisional,
  knownTools,
  target,
  generation,
  reference,
  onReference,
  expanded,
  prose,
  toggle,
  extra,
  onMoreParts,
  found,
}: Props) {
  const next = extra ? extra.next : entry?.partsCursor;
  const sourceParts = [...(entry?.parts ?? []), ...(extra?.items ?? [])];
  const parts = entry ? groupParts(entry, sourceParts, knownTools) : [];
  const wholeEntry =
    entry?.contentRef && !sourceParts.some((part) => part.contentRef === entry.contentRef)
      ? entry.contentRef
      : undefined;
  const entryFormat = parts.some((group) => group.kind === "prose") ? "markdown" : "text";
  const singlePart = sourceParts.length === 1 ? sourceParts[0] : undefined;
  const singleRef = entry && singlePart && inlineProseReference(entry, singlePart);
  const wholeIsInline = singleRef && !reference && prose.get(singleRef) != null;

  function disclosure(
    entryId: string,
    ref: string,
    format: "markdown" | "text",
    entire = false,
    match?: FoundOutput,
  ) {
    const open = reference === ref;
    return (
      <div className="stack">
        <button
          type="button"
          className="output-disclosure quiet-button"
          onClick={() => onReference(open ? undefined : ref)}
          aria-expanded={open}
        >
          {entire
            ? open
              ? "Close complete entry"
              : "Read complete entry"
            : open
              ? "Close full output"
              : "Read full output"}
        </button>
        {open && (
          <ExpandedOutput
            key={match ? ref + ":" + match.destination.offset : ref}
            target={target}
            generation={generation}
            entryId={entryId}
            reference={ref}
            format={format}
            {...(match ? { found: match } : {})}
          />
        )}
      </div>
    );
  }

  function value(current: HistoryEntry, part: HistoryPart, key: string) {
    if (found && found.destination.nativeIndex === (part.nativeIndex ?? null)) {
      const { contentRef, markdown, nativeIndex } = found.destination;
      return (
        <div className="stack" key={key}>
          {disclosure(
            current.entryId,
            contentRef,
            markdown ? "markdown" : "text",
            nativeIndex === null,
            found,
          )}
        </div>
      );
    }
    const formatted = part.kind !== "tool_call" && part.kind !== "tool_result";
    const inlineRef = inlineProseReference(current, part);
    const complete = inlineRef && !reference ? prose.get(inlineRef) : undefined;
    const sameReply = part === singlePart && part.kind === "assistant_text" && !next;
    return (
      <div className="stack" key={key}>
        {(!part.contentRef || reference !== part.contentRef) &&
          (formatted ? (
            <Prose
              key={sameReply ? current.turnId : undefined}
              complete={complete || undefined}
              display={"display" in part ? part.display : undefined}
              preview={part.text}
              waiting={Boolean(sameReply && inlineRef && !reference && complete === undefined)}
            />
          ) : (
            <pre className="tool-output-line">{part.text}</pre>
          ))}
        {part.contentRef &&
          complete == null &&
          disclosure(current.entryId, part.contentRef, formatted ? "markdown" : "text")}
      </div>
    );
  }

  const body = entry
    ? parts.map((group, index) => {
        if (group.kind === "prose") return value(entry, group.part, `part:${index}`);
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
                {group.parts.map((item, ordinal) => value(entry, item, `tool:${ordinal}`))}
              </div>
            )}
          </details>
        );
      })
    : provisional
      ? [
          <div className="stack" key="part:0">
            <Prose key={provisional.turnId} preview={provisional.text} provisional />
          </div>,
        ]
      : [];
  return (
    <>
      {body}
      {entry &&
        wholeEntry &&
        !wholeIsInline &&
        !found &&
        disclosure(entry.entryId, wholeEntry, entryFormat, true)}
      {next && (
        <button className="quiet-button" type="button" onClick={() => onMoreParts(next)}>
          More parts
        </button>
      )}
    </>
  );
}

function Prose({
  complete,
  display,
  preview,
  provisional = false,
  waiting = false,
}: {
  readonly complete?: InlineProse | undefined;
  readonly display?: DocumentDisplay | null | undefined;
  readonly preview: string;
  readonly provisional?: boolean;
  readonly waiting?: boolean;
}) {
  const [lastPreview, setLastPreview] = useState<string>();
  // Native pages can expose a clipped prefix before the same item's complete part arrives.
  const held = waiting && lastPreview?.startsWith(preview) ? lastPreview : undefined;
  const snapshot = provisional ? preview : held;
  if (lastPreview !== snapshot) setLastPreview(snapshot);

  return (
    <>
      {complete?.kind === "prepared" ? (
        <MarkdownContent tree={complete.tree} display={complete.display} />
      ) : (
        <MarkdownContent
          source={complete?.text ?? held ?? preview}
          display={complete?.display ?? display}
          provisional={provisional || held !== undefined}
          formattingLimit={complete?.kind === "formatting-limit"}
        />
      )}
      {held !== undefined && (
        <p className="meta" role="status">
          Live preview · Loading complete reply…
        </p>
      )}
    </>
  );
}
