import type { HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import type { SessionAddress } from "../../../api/history";
import { ExpandedOutput } from "./ExpandedOutput";
import { ToolHeading } from "./ToolActivity";
import { groupParts, openKey } from "./tool-activity";
import type { ToolCatalog } from "./tool-activity";
import { TranscriptMarkdown } from "./TranscriptMarkdown";

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
  const sourceParts = [...entry.parts, ...(extra?.items ?? [])];
  const parts = groupParts(entry, sourceParts, knownTools);
  const wholeEntry =
    entry.contentRef && !sourceParts.some((part) => part.contentRef === entry.contentRef)
      ? entry.contentRef
      : undefined;
  const entryFormat = parts.some((group) => group.kind === "prose") ? "markdown" : "text";

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
    return (
      <div className="stack" key={key}>
        {(!part.contentRef || reference !== part.contentRef) &&
          (formatted ? (
            <TranscriptMarkdown source={part.text} />
          ) : (
            <pre className="tool-output-line">{part.text}</pre>
          ))}
        {part.contentRef && disclosure(part.contentRef, formatted ? "markdown" : "text")}
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
      {wholeEntry && disclosure(wholeEntry, entryFormat, true)}
      {next && (
        <button className="quiet-button" type="button" onClick={() => onMoreParts(next)}>
          More parts
        </button>
      )}
    </>
  );
}
