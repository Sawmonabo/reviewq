import { useState } from "react";
import { historyPage } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import { useResource } from "../../../hooks/useResource";
import { ExpandedOutput } from "./ExpandedOutput";

export default function SessionResult({ target }: { readonly target: SessionAddress }) {
  const [cursor, setCursor] = useState<string>();
  const page = useResource(
    `result:${target.runId}:${target.sessionKey}:${cursor ?? ""}`,
    (signal) => historyPage(target, cursor ? { cursor } : {}, signal),
    [
      { kind: "history", id: target.sessionKey },
      { kind: "session", id: target.sessionKey },
    ],
  );
  const entry = page.data?.items
    .slice()
    .reverse()
    .find((item) => item.parts.some((part) => part.kind === "assistant_text"));
  const result = entry?.parts
    .slice()
    .reverse()
    .find((part) => part.kind === "assistant_text");
  const older = page.data?.nextCursor;

  if (page.error && !page.data) return <Notice retry={page.retry}>{page.error}</Notice>;
  if (!page.data) return <Loading label="Loading result…" />;
  if (page.data.availability === "unavailable")
    return <Notice retry={page.retry}>The native result source is unavailable.</Notice>;

  return (
    <section className="session-result stack" aria-label="Result">
      {page.error && <Notice retry={page.retry}>{page.error}</Notice>}
      {page.data.availability === "partial" && (
        <Notice>Only part of this native session is available.</Notice>
      )}
      {entry && result ? (
        <>
          <h3>Result</h3>
          {result.contentRef ? (
            <ExpandedOutput
              key={result.contentRef}
              target={target}
              entryId={entry.entryId}
              generation={page.data.generation}
              reference={result.contentRef}
              format="markdown"
            />
          ) : (
            <MarkdownContent source={result.text} />
          )}
        </>
      ) : (
        <Empty title={older ? "No result in this part of the conversation" : "No result yet"}>
          <p>The child’s Conversation contains its available activity.</p>
        </Empty>
      )}
      {(cursor || (!result && older)) && (
        <div className="actions">
          {cursor && <button onClick={() => setCursor(undefined)}>Latest activity</button>}
          {!result && older && (
            <button onClick={() => setCursor(older)}>Look in earlier activity</button>
          )}
        </div>
      )}
    </section>
  );
}
