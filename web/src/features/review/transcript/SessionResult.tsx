import { historyResult } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import { useResource } from "../../../hooks/useResource";
import { ExpandedOutput } from "./ExpandedOutput";

export function useSessionResult(target: SessionAddress | undefined) {
  return useResource(
    target ? `result:${target.runId}:${target.sessionKey}` : "result:none",
    (signal) =>
      target ? historyResult(target, signal) : Promise.resolve({ data: null, cursor: "" }),
    target
      ? [
          { kind: "history", id: target.sessionKey },
          { kind: "session", id: target.sessionKey },
        ]
      : [],
  );
}

export default function SessionResult({
  target,
  resource,
}: {
  readonly target: SessionAddress;
  readonly resource: ReturnType<typeof useSessionResult>;
}) {
  const value = resource.data;
  if (resource.error && !value) return <Notice retry={resource.retry}>{resource.error}</Notice>;
  if (!value) return <Loading label="Loading result…" />;
  if (value.availability === "unavailable")
    return <Notice retry={resource.retry}>The native result source is unavailable.</Notice>;

  const entry = value.entry;
  const response = entry?.parts.find((part) => part.kind === "assistant_text");
  const generation = value.generation;
  const reference = response?.contentRef ?? entry?.contentRef;

  return (
    <section className="session-result stack" aria-label="Result">
      {resource.error && <Notice retry={resource.retry}>{resource.error}</Notice>}
      {value.availability === "partial" && (
        <Notice>Only part of this native session is available.</Notice>
      )}
      {value.failure && (
        <Notice>
          <strong>{value.failure.followedByLeadReport ? "Failed earlier" : "Failed"}</strong>
          <br />
          {value.failure.followedByLeadReport
            ? "The lead returned a response after this subagent failed."
            : "This subagent reported a failed turn."}
        </Notice>
      )}
      {entry && response ? (
        <>
          <h3>Result</h3>
          {reference && generation ? (
            <ExpandedOutput
              key={JSON.stringify([entry.entryId, generation, reference])}
              target={target}
              entryId={entry.entryId}
              generation={generation}
              reference={reference}
              format="markdown"
            />
          ) : (
            <MarkdownContent source={response.text} />
          )}
        </>
      ) : (
        <Empty title="No result yet">
          <p>The child’s Conversation contains its available activity.</p>
        </Empty>
      )}
    </section>
  );
}
