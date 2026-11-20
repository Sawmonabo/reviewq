import type { HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import type { SessionAddress } from "../../../api/history";
import { ExpandedOutput } from "./ExpandedOutput";
import { ToolHeading } from "./ToolActivity";
import { groupParts, openKey } from "./tool-activity";
import type { ToolCatalog } from "./tool-activity";
import {
  MarkdownContent,
  MarkdownSource,
  MarkdownTree,
} from "../../../components/markdown/MarkdownContent";
import { inlineProseReference } from "./useInlineProse";
import type { InlineProse } from "./useInlineProse";

type Props = {
  readonly target: SessionAddress;
  readonly entry: HistoryEntry;
  readonly knownTools: ToolCatalog;
  readonly generation: string;
  readonly reference: string | undefined;
  readonly onReference: (reference: string | undefined) => void;
  readonly expanded: ReadonlySet<string>;
  readonly prose: ReadonlyMap<string, InlineProse | null>;
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
  prose,
  toggle,
  extra,
  onMoreParts,
}: Props) {
  const next = extra ? extra.next : entry.partsCursor;
  const sourceParts = [...entry.parts, ...(extra?.items ?? [])];
  const parts = groupParts(entry, sourceParts, knownTools);
  const wholeEntry =
    entry.contentRef && !sourceParts.some((part) => part.contentRef === entry.contentRef)
      ? entry.contentRef
      : undefined;
  const entryFormat = parts.some((group) => group.kind === "prose") ? "markdown" : "text";
  const singlePart = sourceParts.length === 1 ? sourceParts[0] : undefined;
  const singleRef = singlePart && inlineProseReference(entry, singlePart);
  const wholeIsInline = singleRef && !reference && prose.get(singleRef) != null;

  function disclosure(ref: string, format: "markdown" | "text", entire = false) {
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
            key={ref}
            target={target}
            generation={generation}
            entryId={entry.entryId}
            reference={ref}
            format={format}
          />
        )}
      </div>
    );
  }

  function value(part: HistoryPart, key: string) {
    const formatted = part.kind !== "tool_call" && part.kind !== "tool_result";
    const inlineRef = inlineProseReference(entry, part);
    const complete = inlineRef && !reference ? prose.get(inlineRef) : undefined;
    return (
      <div className="stack" key={key}>
        {(!part.contentRef || reference !== part.contentRef) &&
          (formatted ? (
            <Prose complete={complete || undefined} preview={part.text} />
          ) : (
            <pre className="tool-output-line">{part.text}</pre>
          ))}
        {part.contentRef &&
          complete == null &&
          disclosure(part.contentRef, formatted ? "markdown" : "text")}
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
      {wholeEntry && !wholeIsInline && disclosure(wholeEntry, entryFormat, true)}
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
  preview,
}: {
  readonly complete: InlineProse | undefined;
  readonly preview: string;
}) {
  if (complete?.kind === "prepared") return <MarkdownTree tree={complete.tree} />;
  if (complete?.kind === "formatting-limit") return <MarkdownSource source={complete.text} />;
  return <MarkdownContent source={complete?.text ?? preview} />;
}
