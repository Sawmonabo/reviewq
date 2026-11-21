import type { DiscussionDestination } from "../../../api/generated/contracts";
import {
  nativeRange,
  revealAcross,
  revealDetails,
} from "../../../components/markdown/reading-range";

const highlightName = "reviewq-discussion-match";

export function noteMatch(root: HTMLElement, note: DiscussionDestination) {
  const article = root.querySelector<HTMLElement>(
    `[data-selected-note="${CSS.escape(note.noteId)}"]`,
  );
  const body = article?.querySelector<HTMLElement>(":scope > .prose");
  const range = body && nativeRange(body, { ...note, markdown: true });
  if (!article || !body) return;
  if (!range) return null;
  revealDetails(range.startContainer, body);
  revealDetails(range.endContainer, body);
  if (!revealAcross(range, body)) return null;
  return { article, range };
}

export function highlightNote(range?: Range) {
  if (typeof CSS.highlights === "undefined" || typeof Highlight === "undefined") return;
  if (range) CSS.highlights.set(highlightName, new Highlight(range));
  else CSS.highlights.delete(highlightName);
}
