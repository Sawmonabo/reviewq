import { useEffect, useLayoutEffect, useRef, useState } from "react";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import { unified } from "unified";
import type { PatchDiscussion, PatchDiscussionNote } from "../../../api/generated/contracts";
import { ChangesThread } from "./ChangesThread";

type CompactNotes = "all" | ReadonlySet<string>;
type ReadingAnchor = { viewport: Element; target: Element | Range; top: number };
type MarkdownNode = {
  readonly type: string;
  readonly value?: string | undefined;
  readonly alt?: string | null | undefined;
  readonly children?: readonly MarkdownNode[] | undefined;
};

const markdown = unified().use(remarkParse).use(remarkGfm);
const previews = new WeakMap<PatchDiscussionNote, string>();
const MARKDOWN_SYNTAX = /[*_`~[<>&\\|#]|^(?:[ \t]*[+-]|\d+[.)])[ \t]/m;

export function OtherDiscussions({ threads }: { readonly threads: readonly PatchDiscussion[] }) {
  const group = useRef<HTMLDivElement>(null);
  const anchor = useRef<ReadingAnchor | undefined>(undefined);
  const [compact, setCompact] = useState<CompactNotes>("all");

  useEffect(() => {
    const element = group.current;
    const viewport = element?.closest(".drawer-body");
    if (!element || !viewport) return;

    let nearby = false;
    let shown: CompactNotes = "all";
    let frame = 0;
    function reconcile() {
      frame = 0;
      if (!element || !viewport || !document.hasFocus()) return;
      const selection = document.getSelection();
      const range =
        selection?.rangeCount && !selection.isCollapsed ? selection.getRangeAt(0) : undefined;
      const selected = range !== undefined && range.intersectsNode(element);
      if (!nearby && (selected || element.contains(document.activeElement))) return;
      const next = nearby
        ? new Set(
            selected
              ? Array.from(element.querySelectorAll<HTMLElement>("[data-discussion-source]"))
                  .filter((note) => range.intersectsNode(note))
                  .map((note) => note.dataset.discussionSource!)
              : [],
          )
        : "all";
      if (sameNotes(shown, next)) return;
      anchor.current = readingAnchor(element, viewport, selected ? range : undefined);
      shown = next;
      setCompact(next);
    }
    // On return from the browser Find toolbar the browser can restore the selection.
    function schedule() {
      if (!frame && (nearby || shown !== "all")) frame = requestAnimationFrame(reconcile);
    }
    const observer = new IntersectionObserver(
      ([match]) => {
        nearby = Boolean(match?.isIntersecting);
        schedule();
      },
      { root: viewport, rootMargin: "640px 0px" },
    );
    observer.observe(element);
    element.addEventListener("focusin", schedule);
    element.addEventListener("focusout", schedule);
    document.addEventListener("selectionchange", schedule);
    window.addEventListener("focus", schedule);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(frame);
      element.removeEventListener("focusin", schedule);
      element.removeEventListener("focusout", schedule);
      document.removeEventListener("selectionchange", schedule);
      window.removeEventListener("focus", schedule);
    };
  }, []);

  useLayoutEffect(() => {
    const saved = anchor.current;
    anchor.current = undefined;
    if (!saved) return;
    const { target, viewport, top } = saved;
    const owner = target instanceof Range ? target.commonAncestorContainer : target;
    if (!owner.isConnected) return;
    const delta = target.getBoundingClientRect().top - viewport.getBoundingClientRect().top - top;
    if (Math.abs(delta) >= 1) viewport.scrollTop += delta;
  }, [compact]);

  return (
    <div className="stack" ref={group}>
      {threads.map((thread) => (
        <div className="changes-conversation" key={thread.discussionId}>
          <ChangesThread
            thread={thread}
            outside={!thread.outdated && Boolean(thread.anchor)}
            previewNote={(note) =>
              compact === "all" || compact.has(note.noteId) ? discussionText(note) : undefined
            }
          />
        </div>
      ))}
    </div>
  );
}

function sameNotes(current: CompactNotes, next: CompactNotes) {
  return (
    current === next ||
    (current !== "all" &&
      next !== "all" &&
      current.size === next.size &&
      [...current].every((id) => next.has(id)))
  );
}

function readingAnchor(
  group: Element,
  viewport: Element,
  range?: Range,
): ReadingAnchor | undefined {
  const clip = viewport.getBoundingClientRect();
  const groupRect = group.getBoundingClientRect();
  if (groupRect.bottom <= clip.top || groupRect.top >= clip.bottom) return;
  if (range && group.contains(range.startContainer)) {
    const point = range.cloneRange();
    point.collapse(true);
    const top = point.getBoundingClientRect().top;
    if (top >= clip.top && top < clip.bottom)
      return { viewport, target: point, top: top - clip.top };
  }
  const match = Array.from(group.querySelectorAll("summary, article")).find((entry) => {
    const rect = entry.getBoundingClientRect();
    return rect.bottom > clip.top && rect.top < clip.bottom;
  });
  if (match) return { viewport, target: match, top: match.getBoundingClientRect().top - clip.top };
}

function discussionText(note: PatchDiscussionNote): string {
  // Ordinary unformatted notes already have the exact searchable text.
  if (!MARKDOWN_SYNTAX.test(note.body)) return note.body;
  const retained = previews.get(note);
  if (retained !== undefined) return retained;
  // Read the semantic text from the existing Markdown parser without building the rich HTML view.
  let text = note.body;
  try {
    text = markdownText(markdown.parse(note.body));
  } catch (error) {
    // The original note is still searchable when deeply nested Markdown exceeds the renderer limit.
    if (!(error instanceof RangeError)) throw error;
  }
  previews.set(note, text);
  return text;
}

function markdownText(node: MarkdownNode, depth = 0): string {
  if (depth > 128) throw new RangeError("Discussion formatting depth");
  switch (node.type) {
    case "definition":
      return "";
    case "image":
    case "imageReference":
      return node.alt || "Attachment";
    case "break":
      return "\n";
    case "html": {
      // Template contents remain inert and never enter the page.
      const template = document.createElement("template");
      template.innerHTML = node.value ?? "";
      for (const hidden of template.content.querySelectorAll("script, style, template"))
        hidden.remove();
      return template.content.textContent ?? "";
    }
    default: {
      const separator =
        node.type === "tableRow"
          ? "\t"
          : ["root", "list", "listItem", "blockquote", "table"].includes(node.type)
            ? "\n"
            : "";
      return (
        node.value ??
        node.children
          ?.map((child) => markdownText(child, depth + 1))
          .filter(Boolean)
          .join(separator) ??
        ""
      );
    }
  }
}
