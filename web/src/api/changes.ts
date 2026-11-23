import { decodeUtf8, readBoundedBytes } from "./bounded-bytes";
import { ApiResponseError, queryString, read, request } from "./client";
import type {
  PatchDiscussionNote,
  PatchDiscussionText,
  PatchFile,
  PatchFileIndex,
  PatchPath,
  PatchSourcePage,
} from "./generated/contracts";
import {
  isApprovalViewResponse,
  isCurrentPatchRevisionResponse,
  isPatchDiscussionsResponse,
  isPatchFileIndexResponse,
  isPatchPathsResponse,
  isPatchSourceResponse,
} from "./generated/validators.mjs";

export const PATCH_WINDOW_BYTES = 64 * 1024;
export const PATCH_SOURCE_WINDOW_BYTES = 128 * 1024;
export const PATCH_FILES_PER_PAGE = 10;
export const PATH_MATCHES_PER_PAGE = 60;
const MAX_RICH_BYTES = 2 * 1024 * 1024;
const PATH_CACHE_BYTES = 768 * 1024;
const RANGE_NOT_SATISFIABLE = 416;
export type PatchIndex = PatchFileIndex;
type PatchIndexResult = { readonly cursor: string; readonly data: PatchIndex };
type FileIndexPosition = { readonly revision: string } & (
  | { readonly cursor: string; readonly previewEdge?: "start" | "end" }
  | { readonly path: PatchPath }
  | { readonly fileId: string }
);
// A background edge read cannot discard a selected body awaiting its first paint.
let selectedPreview: { readonly file: PatchFile; readonly text: string } | undefined;
let edgePreviews = new WeakMap<PatchFile, string>();
let retainedIndex: { readonly reviewId: string; readonly result: PatchIndexResult } | undefined;
let retainedCatalog:
  | {
      readonly reviewId: string;
      readonly revision: string;
      readonly result: Awaited<ReturnType<typeof loadPatchPathCatalog>>;
    }
  | undefined;

export async function readPatchFiles(
  reviewId: string,
  signal: AbortSignal,
  position?: FileIndexPosition,
  preferredFileId?: string,
) {
  const query = new URLSearchParams({ limit: String(PATCH_FILES_PER_PAGE) });
  if (position) {
    query.set("revision", position.revision);
    if ("cursor" in position) {
      query.set("cursor", position.cursor);
      if (position.previewEdge) query.set("previewEdge", position.previewEdge);
    } else if ("path" in position) query.set("filePathHint", position.path.lookupKey);
    else query.set("preferredFileId", position.fileId);
  }
  if (preferredFileId) query.set("preferredFileId", preferredFileId);
  const includeSelected = !position || !("cursor" in position);
  if (includeSelected) query.set("includeSelectedPreview", "1");
  const result = await read(
    `${changesRoute(reviewId)}/files?${query}`,
    isPatchFileIndexResponse,
    signal,
  );
  const { data } = result;
  if (
    (position && data.revision !== position.revision) ||
    (position &&
      "path" in position &&
      !data.items.some(
        (file) => file.oldPath === position.path.oldPath && file.newPath === position.path.newPath,
      )) ||
    (position &&
      "fileId" in position &&
      !data.items.some((file) => file.fileId === position.fileId)) ||
    data.items.some((item) => item.reviewId !== reviewId || item.revision !== data.revision)
  ) {
    throw new Error("The selected patch index changed.");
  }
  signal.throwIfAborted();
  if (includeSelected) selectedPreview = undefined;
  else edgePreviews = new WeakMap();
  const selectedId = includeSelected
    ? (preferredFileId ??
      (position && "fileId" in position
        ? position.fileId
        : position && "path" in position
          ? data.items.find(
              (item) =>
                item.oldPath === position.path.oldPath && item.newPath === position.path.newPath,
            )?.fileId
          : data.items.find((item) => item.availability === "complete")?.fileId))
    : undefined;
  let bytesAvailable = 64 * 1024;
  let admitted = 0;
  const items = data.items.map((item) => {
    if (item.preview === undefined || item.preview === null) return item;
    const { preview, ...file } = item;
    const bytes = file.byteLength;
    const selected = includeSelected && file.fileId === selectedId;
    const edge = position && "previewEdge" in position && position.previewEdge;
    if (
      !isRichPatch(file) ||
      bytes === null ||
      (selected
        ? bytes > 128 * 1024 || admitted > 0
        : !edge || bytes > 32 * 1024 || bytes > bytesAvailable || admitted >= 3) ||
      new TextEncoder().encode(preview).length !== bytes
    )
      throw new Error("The selected patch preview exceeded its bounds.");
    admitted++;
    if (selected) selectedPreview = { file, text: preview };
    else {
      edgePreviews.set(file, preview);
      bytesAvailable -= bytes;
    }
    return file;
  });
  const page = { cursor: result.cursor, data: { ...data, items } };
  if (!position) retainedIndex = { reviewId, result: page };
  return page;
}

export async function readInitialPatchFiles(
  reviewId: string,
  signal: AbortSignal,
  preferredFileId: string | undefined,
): Promise<PatchIndexResult> {
  const previous = retainedIndex?.reviewId === reviewId ? retainedIndex.result : undefined;
  if (
    previous &&
    previous.data.settingsRevision != null &&
    (preferredFileId
      ? previous.data.items.some((file) => file.fileId === preferredFileId)
      : previous.data.pageNumber === 1)
  ) {
    try {
      const current = await readPatchRevision(reviewId, signal);
      if (
        current.data.revision === previous.data.revision &&
        current.data.settingsRevision === previous.data.settingsRevision
      ) {
        return { cursor: current.cursor, data: previous.data };
      }
    } catch {
      signal.throwIfAborted();
    }
  }
  return readPatchFiles(reviewId, signal, undefined, preferredFileId);
}

export async function readPatchPathCatalog(
  reviewId: string,
  revision: string,
  signal: AbortSignal,
) {
  if (retainedCatalog?.reviewId === reviewId && retainedCatalog.revision === revision) {
    signal.throwIfAborted();
    return retainedCatalog.result;
  }
  const result = await loadPatchPathCatalog(reviewId, revision, signal);
  signal.throwIfAborted();
  retainedCatalog = { reviewId, revision, result };
  return result;
}

async function loadPatchPathCatalog(reviewId: string, revision: string, signal: AbortSignal) {
  let paths: PatchPath[] | undefined = [];
  let bytes = 0;
  const result = await scanPatchPaths(reviewId, revision, signal, (path) => {
    if (!paths) return;
    bytes += 128 + 2 * (path.oldPath.length + path.newPath.length + path.lookupKey.length);
    if (bytes > PATH_CACHE_BYTES) paths = undefined;
    else paths.push(path);
  });
  return { cursor: result.cursor, data: { ...result.data, paths } };
}

export async function readPatchPathMatches(
  reviewId: string,
  revision: string,
  signal: AbortSignal,
  matches: (path: PatchPath) => boolean,
  offset: number,
) {
  const paths: PatchPath[] = [];
  let count = 0;
  const result = await scanPatchPaths(reviewId, revision, signal, (path) => {
    if (!matches(path)) return;
    if (count >= offset && paths.length < PATH_MATCHES_PER_PAGE) paths.push(path);
    count++;
  });
  return { cursor: result.cursor, data: { count, paths } };
}

export async function locatePatchPath(
  reviewId: string,
  revision: string,
  target: Pick<PatchPath, "oldPath" | "newPath">,
  signal: AbortSignal,
) {
  let selected: PatchPath | undefined;
  await scanPatchPaths(reviewId, revision, signal, (path) => {
    if (path.oldPath !== target.oldPath || path.newPath !== target.newPath) return;
    selected = path;
    return true;
  });
  signal.throwIfAborted();
  return selected;
}

async function scanPatchPaths(
  reviewId: string,
  revision: string,
  signal: AbortSignal,
  visit: (path: PatchPath) => void | boolean,
) {
  const query = new URLSearchParams({ revision });
  let count = 0;
  for (let page = 0; page < 100; page++) {
    const { cursor, data } = await read(
      `${changesRoute(reviewId)}/paths?${query}`,
      isPatchPathsResponse,
      signal,
    );
    if (data.reviewId !== reviewId || data.revision !== revision) {
      throw new Error("The selected file catalog changed.");
    }
    count += data.items.length;
    const found = data.items.some((path) => visit(path) === true);
    if (found || !data.nextCursor) return { cursor, data: { count, partial: data.partial } };
    query.set("cursor", data.nextCursor);
  }
  throw new Error("ReviewQ could not finish reading the file catalog.");
}

export async function readPatchRevision(reviewId: string, signal: AbortSignal) {
  const result = await read(
    `${changesRoute(reviewId)}/revision`,
    isCurrentPatchRevisionResponse,
    signal,
  );
  if (result.data.reviewId !== reviewId) throw new Error("The selected review changed.");
  return result;
}

const discussionText = new WeakMap<PatchDiscussionNote, string>();

export function patchDiscussionText(note: PatchDiscussionNote) {
  return discussionText.get(note);
}

export async function readPatchDiscussions(
  reviewId: string,
  revision: string,
  signal: AbortSignal,
  cursor?: string,
  includeTextProjection = false,
  position?: { readonly anchor?: string | undefined },
) {
  const query = queryString({
    revision,
    cursor,
    recheck: position ? true : undefined,
    anchor: position?.anchor,
  });
  const wantsText = includeTextProjection && !cursor;
  const result = await read(
    `${changesRoute(reviewId)}/discussions${query}`,
    isPatchDiscussionsResponse,
    signal,
    undefined,
    wantsText ? { "X-ReviewQ-Discussion-Text": "1" } : undefined,
  );
  const { data } = result;
  if (data.reviewId !== reviewId || data.revision !== revision) {
    throw new Error("The selected discussion scope changed.");
  }
  if (!data.textProjections) return result;
  const { textProjections, ...page } = data;
  if (wantsText) {
    signal.throwIfAborted();
    rememberDiscussionText(
      page.items.flatMap((thread) => thread.notes),
      textProjections,
    );
  }
  return { ...result, data: page };
}

function rememberDiscussionText(
  notes: readonly PatchDiscussionNote[],
  projections: readonly PatchDiscussionText[],
) {
  const source = new Map(notes.map((note) => [note.noteId, note]));
  const seen = new Set<string>();
  let characters = 0;
  for (const item of projections) {
    characters += item.text.length;
    if (!source.has(item.noteId) || seen.has(item.noteId) || characters > 64 * 1024) return;
    seen.add(item.noteId);
  }
  if (source.size !== notes.length) return;
  for (const item of projections) discussionText.set(source.get(item.noteId)!, item.text);
}

export function downloadPatchUrl(file: PatchFile): string {
  return `/api/v1${patchRoute(file, true)}`;
}

export function isRichPatch(file: PatchFile): boolean {
  return (
    file.availability === "complete" &&
    file.byteLength !== null &&
    file.byteLength <= MAX_RICH_BYTES &&
    file.lineCount !== null &&
    file.lineCount <= 20_000 &&
    file.maxLineBytes !== null &&
    file.maxLineBytes <= 16 * 1024 &&
    file.patchETag !== null
  );
}

export type PatchWindow = {
  readonly text: string;
  readonly first: number;
  readonly next: number;
  readonly total: number;
  readonly etag: string;
};

export type PatchRange = { readonly etag?: string } & (
  { readonly offset: number } | { readonly before: number } | { readonly last: true }
);

export class PatchRangeUnavailable extends Error {
  constructor(
    readonly reportedLength: number | undefined,
    cause?: ApiResponseError,
  ) {
    super(cause?.message ?? "The requested byte range is outside the available patch.", {
      cause,
    });
  }
}

export type PatchSourceTarget =
  | { readonly row: number; readonly byte?: number }
  | { readonly byte: number }
  | { readonly side: "old" | "new"; readonly line: number }
  | { readonly before: number }
  | { readonly last: true };

class PatchSourceChanged extends Error {}

export function isPatchScopeChanged(error: unknown) {
  return (
    error instanceof PatchSourceChanged ||
    (error instanceof ApiResponseError &&
      (error.code === "stale_revision" || error.code === "precondition_failed"))
  );
}

export async function readPatchSource(
  file: PatchFile,
  signal: AbortSignal,
  target: PatchSourceTarget,
): Promise<PatchSourcePage> {
  if (!file.patchETag) throw new Error("The selected patch has no source validator.");
  const query = queryString({
    revision: file.revision,
    fileId: file.fileId,
    ...("last" in target ? { last: 1 } : target),
  });
  const { data } = await read(
    `${changesRoute(file.reviewId)}/source${query}`,
    isPatchSourceResponse,
    signal,
    PATCH_SOURCE_WINDOW_BYTES,
    { "If-Match": file.patchETag },
  );
  if (
    data.reviewId !== file.reviewId ||
    data.fileId !== file.fileId ||
    data.revision !== file.revision ||
    data.patchETag !== file.patchETag ||
    data.totalBytes !== file.byteLength ||
    data.totalRows !== file.lineCount
  )
    throw new PatchSourceChanged("The selected patch source changed.");
  if (!sourceWindowMatches(data, target)) throw new Error("The patch source range is invalid.");
  signal.throwIfAborted();
  return data;
}

function sourceWindowMatches(data: PatchSourcePage, target: PatchSourceTarget): boolean {
  const first = data.rows[0];
  if (first === undefined)
    return (
      data.next === null &&
      ("row" in target
        ? target.row === data.totalRows &&
          (target.byte === undefined || target.byte === data.totalBytes)
        : "byte" in target
          ? target.byte === data.totalBytes
          : "last" in target && data.totalBytes === 0)
    );
  const bytes = new TextEncoder();
  let previous: typeof first | undefined;
  for (const row of data.rows) {
    if (
      row.row >= data.totalRows ||
      row.rowStart > row.sourceStart ||
      row.sourceEnd <= row.sourceStart ||
      row.sourceEnd > row.rowEnd ||
      row.rowEnd > data.totalBytes ||
      (row.hunk !== null && row.hunk > row.rowStart) ||
      row.oldLine === 0 ||
      row.newLine === 0 ||
      bytes.encode(row.text).byteLength !== row.sourceEnd - row.sourceStart ||
      (previous !== undefined &&
        (row.row !== previous.row + 1 ||
          row.rowStart !== previous.sourceEnd ||
          row.rowStart !== previous.rowEnd ||
          row.sourceStart !== row.rowStart))
    )
      return false;
    previous = row;
  }
  const last = previous!;
  const nextRow = last.row + Number(last.rowEnd === last.sourceEnd);
  if (
    data.next === null
      ? last.sourceEnd !== data.totalBytes || nextRow !== data.totalRows
      : data.next.byte !== last.sourceEnd ||
        data.next.row !== nextRow ||
        data.next.byte >= data.totalBytes ||
        data.next.row >= data.totalRows
  )
    return false;
  if ("row" in target)
    return first.row === target.row && first.sourceStart === (target.byte ?? first.rowStart);
  if ("byte" in target)
    return (
      first.sourceStart <= target.byte &&
      target.byte - first.sourceStart <= 3 &&
      target.byte < first.rowEnd
    );
  if ("side" in target)
    return (
      first.sourceStart === first.rowStart &&
      first[target.side === "old" ? "oldLine" : "newLine"] === target.line
    );
  return last.sourceEnd === ("before" in target ? target.before : data.totalBytes);
}

export async function readPatch(
  file: PatchFile,
  signal: AbortSignal,
  window?: PatchRange,
): Promise<PatchWindow> {
  const preview = selectedPreview?.file === file ? selectedPreview.text : edgePreviews.get(file);
  if (preview !== undefined && !window && file.patchETag && file.byteLength !== null) {
    const bytes = new TextEncoder().encode(preview);
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    signal.throwIfAborted();
    const hex = Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join("");
    if (file.patchETag !== `"${hex}"`) throw new Error("The patch preview digest changed.");
    enforcePatchRows(bytes);
    if (selectedPreview?.file === file) selectedPreview = undefined;
    edgePreviews.delete(file);
    return {
      text: preview,
      first: 0,
      next: bytes.length,
      total: bytes.length,
      etag: file.patchETag,
    };
  }
  const headers = new Headers();
  const pinned = file.patchETag ?? window?.etag;
  if (pinned) headers.set("If-Match", pinned);
  const before = window && "before" in window ? window.before : undefined;
  const offset =
    window && "offset" in window ? window.offset : Math.max(0, (before ?? 0) - PATCH_WINDOW_BYTES);
  const suffix = window && "last" in window;
  const bounded = window ? PATCH_WINDOW_BYTES : file.byteLength === 0 ? 0 : MAX_RICH_BYTES;
  if (window)
    headers.set(
      "Range",
      suffix ? `bytes=-${bounded}` : `bytes=${offset}-${(before ?? offset + bounded) - 1}`,
    );
  const response = await request(
    patchRoute(file),
    { headers, signal },
    window ? RANGE_NOT_SATISFIABLE : undefined,
  );
  if (response.status === RANGE_NOT_SATISFIABLE) {
    const match = /^bytes \*\/(\d+)$/.exec(response.headers.get("Content-Range") ?? "");
    const length = Number(match?.[1]);
    await response.body?.cancel();
    signal.throwIfAborted();
    throw new PatchRangeUnavailable(Number.isSafeInteger(length) ? length : undefined);
  }
  const etag = response.headers.get("ETag");
  if (
    response.status !== (window ? 206 : 200) ||
    response.headers.get("X-ReviewQ-Review-Id") !== file.reviewId ||
    response.headers.get("X-ReviewQ-Patch-Revision") !== file.revision ||
    response.headers.get("X-ReviewQ-File-Id") !== file.fileId ||
    !response.headers.get("Content-Type")?.startsWith("text/plain") ||
    etag === null ||
    !/^"[0-9a-f]{64}"$/.test(etag) ||
    (pinned && etag !== pinned)
  ) {
    await response.body?.cancel();
    throw new Error("The response did not prove the selected patch identity.");
  }
  if (!window) {
    const bytes = await readBoundedBytes(response, bounded);
    if (bytes.length !== file.byteLength) throw new Error("The selected patch length changed.");
    enforcePatchRows(bytes);
    signal.throwIfAborted();
    const result = {
      text: decodeUtf8(bytes),
      first: 0,
      next: bytes.length,
      total: bytes.length,
      etag,
    };
    return result;
  }
  const range = contentRange(response);
  if (!range) {
    await response.body?.cancel();
    throw new Error("ReviewQ returned an invalid byte range.");
  }
  const [first, end, total] = range;
  if (
    first !== (suffix ? Math.max(0, total - bounded) : offset) ||
    end !== (before ?? Math.min(total, first + bounded)) ||
    (file.byteLength !== null && total !== file.byteLength)
  ) {
    await response.body?.cancel();
    throw new Error("The selected patch range changed.");
  }
  const bytes = await readBoundedBytes(response, bounded);
  if (bytes.length !== end - first) throw new Error("The selected patch range was incomplete.");
  let leading = 0;
  while (leading < 3 && isContinuation(bytes[leading])) leading++;
  // A backwards read can start inside a CRLF. Keep the LF for the
  // preceding range so the pair stays in one text node.
  if ((suffix || before !== undefined) && first > 0 && bytes[leading] === 10) leading++;
  let next = bytes.length;
  if (end < total) {
    next--;
    while (next > leading && isContinuation(bytes[next])) next--;
    const firstByte = bytes[next] ?? 0;
    const width = firstByte < 0x80 ? 1 : firstByte < 0xe0 ? 2 : firstByte < 0xf0 ? 3 : 4;
    if (next + width <= bytes.length) next = bytes.length;
  }
  if (before === undefined && end < total && bytes[next - 1] === 13) next--;
  if (before !== undefined && first + next !== before)
    throw new Error("The selected patch boundary changed.");
  signal.throwIfAborted();
  return {
    text: decodeUtf8(bytes.subarray(leading, next)),
    first: first + leading,
    next: first + next,
    total,
    etag,
  };
}

function contentRange(response: Response): [number, number, number] | undefined {
  const match = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers.get("Content-Range") ?? "");
  if (!match) return;
  const first = Number(match[1]);
  const end = Number(match[2]) + 1;
  const total = Number(match[3]);
  if (![first, end, total].every(Number.isSafeInteger) || first >= end || end > total) return;
  return [first, end, total];
}

function isContinuation(byte: number | undefined): boolean {
  return byte !== undefined && (byte & 0xc0) === 0x80;
}

function changesRoute(reviewId: string): string {
  return `/reviews/${encodeURIComponent(reviewId)}/changes`;
}

function patchRoute(file: PatchFile, download = false): string {
  const query = new URLSearchParams({ fileId: file.fileId, revision: file.revision });
  if (download) query.set("download", "1");
  return `${changesRoute(file.reviewId)}/patch?${query}`;
}

export async function readApprovals(reviewId: string, signal: AbortSignal) {
  const result = await read(`${changesRoute(reviewId)}/approvals`, isApprovalViewResponse, signal);
  if (result.data.reviewId !== reviewId) throw new Error("The approval scope changed.");
  return result;
}

export class PatchFormattingLimit extends Error {}

function enforcePatchRows(bytes: Uint8Array) {
  let rows = 0;
  let start = 0;
  for (let index = 0; index <= bytes.length; index++) {
    if (index === bytes.length && start === index) break;
    if (index === bytes.length || bytes[index] === 10) {
      rows++;
      const width = index - start - (bytes[index - 1] === 13 && bytes[index] === 10 ? 1 : 0);
      if (rows > 20_000 || width > 16_384)
        throw new PatchFormattingLimit("Patch formatting limit reached.");
      start = index + 1;
    }
  }
}
