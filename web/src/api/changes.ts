import { decodeUtf8, readBoundedBytes } from "./bounded-bytes";
import { read, request } from "./client";
import type { PatchFile, PatchFileIndex, PatchPath } from "./generated/contracts";
import {
  isApprovalViewResponse,
  isCurrentPatchRevisionResponse,
  isPatchDiscussionsResponse,
  isPatchFileIndexResponse,
  isPatchPathsResponse,
} from "./generated/validators.mjs";

export const PATCH_WINDOW_BYTES = 64 * 1024;
export const PATCH_FILES_PER_PAGE = 10;
export const PATH_MATCHES_PER_PAGE = 60;
const MAX_RICH_BYTES = 2 * 1024 * 1024;
const PATH_CACHE_BYTES = 768 * 1024;
export type PatchIndex = PatchFileIndex;
type PatchIndexResult = { readonly cursor: string; readonly data: PatchIndex };
type FileIndexPosition = { readonly revision: string } & (
  { readonly cursor: string; readonly previewEdge?: "start" | "end" } | { readonly path: PatchPath }
);
// Keep bodies from only the most recent requested page edge, outside the metadata cache.
let pageBodies = new WeakMap<PatchFile, string>();
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
    } else query.set("filePathHint", position.path.lookupKey);
  }
  if (preferredFileId) query.set("preferredFileId", preferredFileId);
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
    data.items.some((item) => item.reviewId !== reviewId || item.revision !== data.revision)
  ) {
    throw new Error("The selected patch index changed.");
  }
  signal.throwIfAborted();
  pageBodies = new WeakMap();
  let bytesAvailable = 64 * 1024;
  let admitted = 0;
  const items = data.items.map((item) => {
    if (item.preview === undefined || item.preview === null) return item;
    const { preview, ...file } = item;
    const bytes = file.byteLength;
    if (
      !position ||
      !("previewEdge" in position) ||
      !position.previewEdge ||
      !isRichPatch(file) ||
      bytes === null ||
      bytes > 32 * 1024 ||
      bytes > bytesAvailable ||
      admitted >= 3 ||
      new TextEncoder().encode(preview).length !== bytes
    )
      throw new Error("The selected patch preview exceeded its bounds.");
    admitted++;
    pageBodies.set(file, preview);
    bytesAvailable -= bytes;
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
  text: string,
  offset: number,
) {
  const paths: PatchPath[] = [];
  let count = 0;
  const result = await scanPatchPaths(reviewId, revision, signal, (path) => {
    if (!matchesPatchPath(path, text)) return;
    if (count >= offset && paths.length < PATH_MATCHES_PER_PAGE) paths.push(path);
    count++;
  });
  return { cursor: result.cursor, data: { count, paths } };
}

export function matchesPatchPath(path: PatchPath, text: string) {
  return path.oldPath.toLowerCase().includes(text) || path.newPath.toLowerCase().includes(text);
}

async function scanPatchPaths(
  reviewId: string,
  revision: string,
  signal: AbortSignal,
  visit: (path: PatchPath) => void,
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
    data.items.forEach(visit);
    if (!data.nextCursor) return { cursor, data: { count, partial: data.partial } };
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

export async function readPatchDiscussions(
  reviewId: string,
  revision: string,
  signal: AbortSignal,
  cursor?: string,
) {
  const query = new URLSearchParams({ revision });
  if (cursor) query.set("cursor", cursor);
  const result = await read(
    `${changesRoute(reviewId)}/discussions?${query}`,
    isPatchDiscussionsResponse,
    signal,
  );
  const { data } = result;
  if (data.reviewId !== reviewId || data.revision !== revision) {
    throw new Error("The selected discussion scope changed.");
  }
  return result;
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

export async function readPatch(
  file: PatchFile,
  signal: AbortSignal,
  window?: PatchRange,
): Promise<PatchWindow> {
  const preview = pageBodies.get(file);
  if (preview !== undefined && !window && file.patchETag && file.byteLength !== null) {
    const bytes = new TextEncoder().encode(preview);
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    signal.throwIfAborted();
    const hex = Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join("");
    if (file.patchETag !== `"${hex}"`) throw new Error("The patch preview digest changed.");
    enforcePatchRows(bytes);
    pageBodies.delete(file);
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
  const bounded = window === undefined ? MAX_RICH_BYTES : PATCH_WINDOW_BYTES;
  if (window)
    headers.set(
      "Range",
      suffix ? `bytes=-${bounded}` : `bytes=${offset}-${(before ?? offset + bounded) - 1}`,
    );
  const response = await request(patchRoute(file), { headers, signal });
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
  const [first, end, total] = contentRange(response);
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

function contentRange(response: Response): [number, number, number] {
  const match = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers.get("Content-Range") ?? "");
  if (!match) throw new Error("ReviewQ returned an invalid byte range.");
  const first = Number(match[1]);
  const end = Number(match[2]) + 1;
  const total = Number(match[3]);
  if (![first, end, total].every(Number.isSafeInteger) || first >= end || end > total) {
    throw new Error("ReviewQ returned an invalid byte range.");
  }
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
