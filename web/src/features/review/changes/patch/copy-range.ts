import { readPatch } from "../../../../api/changes";
import type { PatchFile } from "../../../../api/generated/contracts";
import { indexAtByte } from "../patch-rows";
import type { PatchReadQueue } from "./read-queue";

export async function copyPatchRange(
  file: PatchFile,
  first: number,
  last: number,
  signal: AbortSignal,
  read: PatchReadQueue,
) {
  const etag = file.patchETag;
  if (!etag || first < 0 || last <= first || last > (file.byteLength ?? 0))
    throw new Error("The selected patch range changed.");
  const parts: Blob[] = [];
  for (let offset = first; offset < last;) {
    const page = await read(signal, "foreground", () => readPatch(file, signal, { offset, etag }));
    if (page.first !== offset || page.next <= offset)
      throw new Error("ReviewQ could not read the selected patch range.");
    const end = page.next > last ? indexAtByte(page.text, last - offset) : page.text.length;
    if (end === undefined) throw new Error("The selected patch boundary changed.");
    parts.push(new Blob([page.text.slice(0, end)]));
    offset = page.next;
  }
  signal.throwIfAborted();
  return new Blob(parts, { type: "text/plain" });
}
