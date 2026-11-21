import type { Root } from "hast";
import type {
  HistoryContent,
  HistoryPageResponse,
  HistoryPart,
} from "../../../../api/generated/contracts";
import type {
  HistorySearchDestination,
  HistorySearchHit,
} from "../../../../api/generated/contracts/history";
import {
  historyContent,
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
import { completeDocument, documentTree } from "../document";

const changed = "This occurrence is no longer available in the conversation.";
const highlightName = "reviewq-transcript-match";

export type FoundOutput = {
  readonly destination: HistorySearchDestination;
  readonly content: HistoryContent;
  readonly tree?: Root;
};

export type PreparedDestination = {
  readonly page: HistoryPageResponse;
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
  async function readBody() {
    const prepared =
      destination.markdown && destination.offset === 0 && (destination.outputBytes ?? 0) >= 8 * 1024
        ? await completeDocument(target, entryId, generation, contentRef, signal)
        : undefined;
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
    return { content, window: prepared?.window };
  }
  const [page, body] = await Promise.all([
    historyPage(target, { aroundId: entryId, locator }, signal),
    readBody(),
  ]);
  const { content } = body;
  const entry = page.data.items.find((row) => row.entryId === entryId);
  if (
    !entry ||
    page.data.generation !== generation ||
    page.data.reset ||
    content.offset !== destination.offset
  )
    throw new Error(changed);
  const tree = destination.markdown
    ? body.window
      ? documentTree(body.window, content.text)
      : parseMarkdown(content.text)
    : undefined;
  if (destination.markdown && (!tree || !content.complete)) throw new Error(changed);
  if (!positions(tree ? markdownText(tree) : content.text, destination)) throw new Error(changed);
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
    output: { destination, content, ...(tree ? { tree } : {}) },
    ...(extra ? { extra } : {}),
  };
}

function revealOccurrence(view: HTMLElement, match: HistorySearchDestination, inset?: number) {
  const row = view.querySelector('[data-native-entry="' + CSS.escape(match.entryId) + '"]');
  const body = row?.querySelector<HTMLElement>("[data-find-body]");
  const range = body && nativeRange(body, match);
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
    const projected = revealOccurrence(view, match, inset);
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
