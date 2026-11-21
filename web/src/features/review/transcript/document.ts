import { ApiResponseError } from "../../../api/client";
import { historyDocument } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import type { HistoryContent, HistoryDocument } from "../../../api/generated/contracts";
import type { DocumentDisplay } from "../../../api/generated/contracts/common";
import { parseMarkdown } from "../../../components/markdown/parse-markdown";
import { tablePageTree } from "../../../components/markdown/table-page";
import { parseSettledMarkdown } from "../../../components/markdown/settled";

export function documentSource(data: HistoryDocument) {
  if (data.result.state === "source") return data.result.markdown;
  if (data.result.state === "table") return data.result.page.markdown;
  return data.result.blocks.map((block) => block.markdown).join("");
}

export function documentDisplay(data: HistoryDocument): DocumentDisplay | undefined {
  if (data.result.state === "source") return data.result.display ?? undefined;
  if (data.result.state !== "ready") return;
  const drawings = data.result.blocks.flatMap((block) => block.display?.drawings ?? []);
  if (drawings.length)
    return { drawings: drawings.slice(0, 8) as NonNullable<DocumentDisplay["drawings"]> };
}

export function documentTree(data: HistoryDocument, source: string) {
  if (data.result.state === "table") return tablePageTree(data.result.page);
  if (data.result.state !== "ready")
    return data.byteStart === 0 && data.byteEnd === data.byteLength
      ? parseMarkdown(source)
      : undefined;
  return parseSettledMarkdown(
    source,
    data.result.blocks.map((block) => ({
      markdown: block.markdown,
      table: block.display?.table,
    })),
  );
}

export async function completeDocument(
  target: SessionAddress,
  entryId: string,
  generation: string,
  reference: string,
  signal: AbortSignal,
): Promise<
  { readonly content: HistoryContent; readonly window: HistoryDocument } | "paged" | undefined
> {
  try {
    const { data } = await historyDocument(
      target,
      entryId,
      generation,
      reference,
      { byteOffset: 0 },
      signal,
    );
    if (data.tableStart != null) return "paged";
    if (data.byteStart !== 0 || data.nextByte !== null) return;
    return {
      window: data,
      content: {
        runId: data.runId,
        sessionKey: data.sessionKey,
        entryId: data.entryId,
        generation: data.generation,
        availability: "available",
        offset: 0,
        text: documentSource(data),
        complete: true,
        nextCursor: null,
        reset: false,
        display: documentDisplay(data) ?? null,
      },
    };
  } catch (error) {
    signal.throwIfAborted();
    if (!unavailableWindow(error)) throw error;
  }
}

export function unavailableWindow(error: unknown) {
  return (
    error instanceof ApiResponseError && ["not_found", "unavailable", "busy"].includes(error.code)
  );
}
