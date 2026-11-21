import { patchDiscussionText } from "../../../api/changes";
import type { PatchDiscussionNote } from "../../../api/generated/contracts";
import { projectMarkdownText } from "../../../components/markdown/parse-markdown";

const previews = new WeakMap<PatchDiscussionNote, string>();

export function discussionPreview(note: PatchDiscussionNote): string {
  const retained = patchDiscussionText(note) ?? previews.get(note);
  if (retained !== undefined) return retained;
  const text = projectMarkdownText(note.body);
  previews.set(note, text);
  return text;
}
