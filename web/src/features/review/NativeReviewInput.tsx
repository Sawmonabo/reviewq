import type { RunDetail } from "../../api/generated/contracts";
import { historyContent, historyPage } from "../../api/history";
import type { SessionAddress } from "../../api/history";
import { MarkdownContent } from "../../components/markdown/MarkdownContent";
import { Notice } from "../../components/PanelState";
import { useResource } from "../../hooks/useResource";

type Input = NonNullable<NonNullable<RunDetail["requestEvidence"]>["input"]>;

export function NativeReviewInput({
  target,
  input,
}: {
  readonly target: SessionAddress;
  readonly input: Input;
}) {
  const request = useResource(
    JSON.stringify(["review-input", target.runId, target.sessionKey, input]),
    (signal) => readInput(target, input, signal),
  );
  if (!request.data && !request.error) return null;
  return (
    <section className="review-request">
      <h3>{input.slot === "review_request" ? "Review request" : "Skill arguments"}</h3>
      {request.error && <Notice retry={request.retry}>{request.error}</Notice>}
      {request.data && <MarkdownContent source={request.data} />}
    </section>
  );
}

async function readInput(target: SessionAddress, input: Input, signal: AbortSignal) {
  const { data, cursor } = await historyPage(target, { aroundId: input.entryId, limit: 1 }, signal);
  const entry = data.items.find(
    (item) => item.entryId === input.entryId && item.turnId === input.turnId,
  );
  if (
    data.reset ||
    data.generation !== input.generation ||
    !entry ||
    entry.partsCursor ||
    !entry.parts.length ||
    !entry.parts.every((part) => part.kind === "user_prompt")
  ) {
    throw new Error("The original review request could not be verified.");
  }
  const reference =
    entry.contentRef ?? (entry.parts.length === 1 ? entry.parts[0]?.contentRef : undefined);
  if (!reference) {
    const text = entry.parts.map((part) => part.text).join("\n");
    if (
      entry.parts.some((part) => part.contentRef) ||
      new TextEncoder().encode(text).length !== entry.textBytes
    )
      throw new Error("The original review request is incomplete.");
    return { data: text, cursor };
  }
  const content = await historyContent(
    target,
    entry.entryId,
    data.generation,
    reference,
    undefined,
    signal,
  );
  if (content.offset !== 0 || !content.complete || content.nextCursor)
    throw new Error("The original review request is incomplete.");
  return { data: content.text, cursor };
}
