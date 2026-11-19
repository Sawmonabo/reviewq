import { historyResult } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import { useResource } from "../../../hooks/useResource";
import { ExpandedOutput } from "./ExpandedOutput";

export default function SessionResult({ target }: { readonly target: SessionAddress }) {
  const resource = useResource(
    `result:${target.runId}:${target.sessionKey}`,
    (signal) => historyResult(target, signal),
    [
      { kind: "history", id: target.sessionKey },
      { kind: "session", id: target.sessionKey },
    ],
  );
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
