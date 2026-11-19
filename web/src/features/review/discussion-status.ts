import type { PatchDiscussion } from "../../api/generated/contracts";

export function discussionStatus(thread: PatchDiscussion) {
  const resolvable = thread.notes.filter((note) => note.resolvable === true);
  if (resolvable.some((note) => note.resolved === false)) return "open";
  if (thread.notes.some((note) => note.resolvable === null)) return "unknown";
  if (!resolvable.length) return "comment";
  return resolvable.every((note) => note.resolved === true) ? "resolved" : "unknown";
}
