import type { Root } from "hast";
import type {
  HistoryContent,
  HistoryDocument,
  HistoryPageResponse,
  HistoryPart,
} from "../../../../api/generated/contracts";
import type {
  HistorySearchDestination,
  HistorySearchHit,
} from "../../../../api/generated/contracts/history";
import {
  canonicalHistoryMarkdown,
  historyContent,
  historyDocument,
  historyPage,
  historyParts,
  historySearchDestination,
} from "../../../../api/history";
import type { SessionAddress } from "../../../../api/history";
import { parseMarkdown } from "../../../../components/markdown/parse-markdown";
import {
  markdownText,
  nativeRange,
  positions,
  revealAcross,
  revealDetails,
} from "../../../../components/markdown/reading-range";
import { completeDocument, documentSource, documentTree } from "../document";
import { historyKey } from "../history-window";

const changed = "This occurrence is no longer available in the conversation.";
const highlightName = "reviewq-transcript-match";

export type FindDocument = {
  readonly window: HistoryDocument;
  readonly source: string;
  readonly tree: Root | undefined;
  readonly whole: boolean;
};

export type FoundOutput = {
  readonly destination: HistorySearchDestination;
} & (
  | { readonly content: HistoryContent; readonly tree?: Root; readonly document?: never }
  | { readonly document: FindDocument; readonly content?: never; readonly tree?: never }
);

export type PreparedDestination = {
  readonly page: HistoryPageResponse;
  readonly rowId: string;
  readonly output: FoundOutput;
  readonly extra?: { readonly items: readonly HistoryPart[]; readonly next: string | null };
};

export async function prepareDestination(
  target: SessionAddress,
  hit: HistorySearchHit,
  signal: AbortSignal,
): Promise<PreparedDestination> {
  const destination = await historySearchDestination(target, hit.hit, signal);
  if (destination.entryId !== hit.entryId || destination.matched !== hit.matched)
    throw new Error(changed);
  const { entryId, generation, locator, contentRef, contentCursor, nativeIndex, partsCursor } =
    destination;
  async function readBody(): Promise<FoundOutput> {
    if (destination.document) {
      const { byteStart, sha256, mode } = destination.document;
      const { data: window } = await historyDocument(
        target,
        entryId,
        generation,
        contentRef,
        { byteOffset: byteStart, sha256 },
        signal,
      );
      if (window.byteStart !== byteStart || window.sha256 !== sha256) throw new Error(changed);
      const source = documentSource(window);
      const whole = mode === "canonical";
      let tree;
      if (whole) {
        if (window.result.state !== "source" || !window.result.canonical) throw new Error(changed);
        tree = parseMarkdown(await canonicalHistoryMarkdown(target, contentRef, window, signal));
      } else if (mode !== "source") {
        tree = documentTree(window, source);
      }
      if (mode !== "source" && !tree) throw new Error(changed);
      if (mode === "table_body" && window.result.state !== "table") throw new Error(changed);
      const text = tree ? markdownText(tree, mode === "table_body") : source;
      if (!positions(text, destination)) throw new Error(changed);
      return { destination, document: { window, source, tree, whole } };
    }
    const settled =
      destination.markdown && destination.offset === 0 && (destination.outputBytes ?? 0) >= 8 * 1024
        ? await completeDocument(target, entryId, generation, contentRef, signal)
        : undefined;
    const prepared = settled === "paged" ? undefined : settled;
    const content =
      prepared?.content ??
      (await historyContent(
        target,
        entryId,
        generation,
        contentRef,
        contentCursor ?? undefined,
        signal,
      ));
    if (content.offset !== destination.offset) throw new Error(changed);
    const tree = destination.markdown
      ? prepared?.window
        ? documentTree(prepared.window, content.text)
        : parseMarkdown(content.text)
      : undefined;
    if (destination.markdown && (!tree || !content.complete)) throw new Error(changed);
    if (!positions(tree ? markdownText(tree) : content.text, destination)) throw new Error(changed);
    return { destination, content, ...(tree ? { tree } : {}) };
  }
  const [expectedPage, output] = await Promise.all([
    historyPage(target, { aroundId: entryId, locator }, signal),
    readBody(),
  ]);
  const occurrences = expectedPage.data.items.filter((row) => row.entryId === entryId);
  const page =
    occurrences.length > 1
      ? await historyPage(target, { aroundId: entryId, locator, limit: 1 }, signal)
      : expectedPage;
  const matching = page.data.items.filter((row) => row.entryId === entryId);
  const entry = matching[0];
  if (!entry || matching.length !== 1 || page.data.generation !== generation || page.data.reset)
    throw new Error(changed);
  let extra;
  if (nativeIndex !== null && !entry.parts.some((part) => part.nativeIndex === nativeIndex)) {
    if (!partsCursor) throw new Error(changed);
    const result = await historyParts(target, entryId, partsCursor, signal);
    if (!result.parts.some((part) => part.nativeIndex === nativeIndex)) throw new Error(changed);
    extra = { items: result.parts, next: result.nextCursor };
  }
  signal.throwIfAborted();
  // The read can race an in-place replacement even when native history keeps the same generation.
  await historySearchDestination(target, hit.hit, signal);
  return {
    page,
    rowId: historyKey(entry),
    output,
    ...(extra ? { extra } : {}),
  };
}

function revealOccurrence(
  view: HTMLElement,
  rowId: string,
  match: HistorySearchDestination,
  inset?: number,
) {
  const row = view.querySelector(`[data-entry="${CSS.escape(rowId)}"]`);
  const body = row?.querySelector<HTMLElement>("[data-find-body]");
  const scope = match.document?.mode === "table_body" ? body?.querySelector("tbody") : body;
  const range = scope && nativeRange(scope, match);
  if (!body || !range) return;
  revealDetails(range.startContainer, body);
  revealDetails(range.endContainer, body);
  if (!revealAcross(range, body)) return;
  const rect = range.getBoundingClientRect();
  if (!rect.height) return;
  const bounds = view.getBoundingClientRect();
  if (inset !== undefined || rect.top < bounds.top + 32 || rect.bottom > bounds.bottom - 12) {
    const movement = rect.top - bounds.top - (inset ?? bounds.height * 0.32);
    if (Math.abs(movement) > 1) view.scrollTop += movement;
  }
  const visible = range.getBoundingClientRect();
  if (
    visible.top < bounds.top - 1 ||
    visible.top >= bounds.bottom ||
    (visible.height <= bounds.height && visible.bottom > bounds.bottom + 1)
  )
    return;
  if (typeof CSS.highlights !== "undefined" && typeof Highlight !== "undefined") {
    CSS.highlights.set(highlightName, new Highlight(range));
  }
  return visible.top - bounds.top;
}

export function holdOccurrence(
  view: HTMLElement,
  rowId: string,
  match: HistorySearchDestination,
  signal: AbortSignal,
  onReveal: () => void,
  onUnavailable: () => void,
) {
  let frame = 0;
  let inset: number | undefined;
  const deadline = performance.now() + 1200;
  const observer = new ResizeObserver(() => {
    if (inset === undefined) return schedule();
    // Layout settled after the selected range was already visible. Correct its inset
    // within this delivery, before the browser paints the shifted content.
    cancelAnimationFrame(frame);
    paint();
  });
  observer.observe(view);
  const spacer = view.querySelector(".timeline-spacer");
  if (spacer) observer.observe(spacer);
  view.addEventListener("scroll", schedule, { passive: true });
  signal.addEventListener("abort", close, { once: true });
  schedule();

  function schedule() {
    if (!frame && !signal.aborted) frame = requestAnimationFrame(paint);
  }

  function paint() {
    frame = 0;
    if (signal.aborted) return;
    const projected = revealOccurrence(view, rowId, match, inset);
    if (projected !== undefined && inset === undefined) {
      inset = projected;
      onReveal();
    }
    if (inset !== undefined) return;
    if (performance.now() < deadline) schedule();
    else {
      close();
      onUnavailable();
    }
  }

  function close() {
    cancelAnimationFrame(frame);
    observer.disconnect();
    view.removeEventListener("scroll", schedule);
    signal.removeEventListener("abort", close);
  }
}

export function clearOccurrence() {
  CSS.highlights?.delete(highlightName);
}
