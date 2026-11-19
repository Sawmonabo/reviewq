import { decodeUtf8, readBoundedBytes } from "./bounded-bytes";
import { read, request } from "./client";
import type { PatchFile, PatchFileIndex, PatchPath } from "./generated/contracts";
import {
  isApprovalViewResponse,
  isCurrentPatchRevisionResponse,
  isPatchDiscussionsResponse,
  isPatchFileIndexResponse,
  isPatchPathsResponse,
  isSelectedPatchResponse,
} from "./generated/validators.mjs";

export const PATCH_WINDOW_BYTES = 64 * 1024;
export const PATCH_FILES_PER_PAGE = 10;
export const PATH_MATCHES_PER_PAGE = 60;
const MAX_RICH_BYTES = 2 * 1024 * 1024;
const PATH_CACHE_BYTES = 768 * 1024;
const RETAINED_PATCH_BYTES = 128 * 1024;
export type PatchIndex = PatchFileIndex & { readonly singleFile?: true };
type PatchIndexResult = { readonly cursor: string; readonly data: PatchIndex };
export type PatchSelection = { readonly revision: string } & (
  { readonly path: PatchPath } | { readonly selectedCursor: string }
);
type RetainedPatch = { readonly identity: string; readonly result: PatchWindow };
type PreparedSelection = {
  readonly index: PatchIndexResult;
  readonly patch: RetainedPatch | undefined;
};
type Prefetch = {
  readonly key: string;
  readonly controller: AbortController;
  readonly result: Promise<PreparedSelection>;
};
let retainedIndex: { readonly reviewId: string; readonly result: PatchIndexResult } | undefined;
let retainedCatalog:
  | {
      readonly reviewId: string;
      readonly revision: string;
      readonly result: Awaited<ReturnType<typeof loadPatchPathCatalog>>;
    }
  | undefined;
let retainedPatch: RetainedPatch | undefined;
let prefetched: Prefetch | undefined;

export async function readPatchFiles(
  reviewId: string,
  signal: AbortSignal,
  position?: { readonly revision: string; readonly cursor: string },
  preferredFileId?: string,
) {
  const query = new URLSearchParams({ limit: String(PATCH_FILES_PER_PAGE) });
  if (position) {
    query.set("revision", position.revision);
    query.set("cursor", position.cursor);
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
    data.items.some((item) => item.reviewId !== reviewId || item.revision !== data.revision)
  ) {
    throw new Error("The selected patch index changed.");
  }
  signal.throwIfAborted();
  retainedIndex = { reviewId, result };
  return result;
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

export async function readSelectedPatch(
  reviewId: string,
  signal: AbortSignal,
  selection: PatchSelection,
  previous: PatchIndex,
  candidate?: Prefetch,
): Promise<PatchIndexResult> {
  let prepared: PreparedSelection;
  if (
    candidate &&
    "path" in selection &&
    candidate.key === prefetchKey(reviewId, previous, selection.path)
  ) {
    const abort = () => candidate.controller.abort();
    signal.addEventListener("abort", abort, { once: true });
    if (signal.aborted) abort();
    try {
      prepared = await candidate.result;
    } finally {
      signal.removeEventListener("abort", abort);
    }
  } else {
    candidate?.controller.abort();
    prepared = await loadSelectedPatch(reviewId, signal, selection, previous);
  }
  signal.throwIfAborted();
  if (prepared.patch) retainedPatch = prepared.patch;
  retainedIndex = { reviewId, result: prepared.index };
  return prepared.index;
}

export function takeSelectedPatchPrefetch(reviewId: string, previous: PatchIndex, path: PatchPath) {
  if (prefetched?.key !== prefetchKey(reviewId, previous, path)) return;
  const candidate = prefetched;
  prefetched = undefined;
  return candidate;
}

export function prefetchSelectedPatch(reviewId: string, previous: PatchIndex, path: PatchPath) {
  prefetched?.controller.abort();
  const controller = new AbortController();
  const result = loadSelectedPatch(
    reviewId,
    controller.signal,
    { revision: previous.revision, path },
    previous,
  );
  const read = { key: prefetchKey(reviewId, previous, path), controller, result };
  prefetched = read;
  void result.catch(() => {
    if (prefetched === read) prefetched = undefined;
  });
  function cancel() {
    window.removeEventListener("pagehide", cancel);
    if (prefetched === read) {
      prefetched = undefined;
      controller.abort();
    }
  }
  window.addEventListener("pagehide", cancel);
  return cancel;
}

function prefetchKey(reviewId: string, previous: PatchIndex, path: PatchPath) {
  return JSON.stringify([reviewId, previous.settingsRevision, previous.revision, path.lookupKey]);
}

async function loadSelectedPatch(
  reviewId: string,
  signal: AbortSignal,
  selection: PatchSelection,
  previous: PatchIndex,
): Promise<PreparedSelection> {
  const { revision } = selection;
  const query = new URLSearchParams({ revision });
  if ("path" in selection) query.set("filePathHint", selection.path.lookupKey);
  else query.set("cursor", selection.selectedCursor);
  const { cursor, data } = await read(
    `${changesRoute(reviewId)}/selected?${query}`,
    isSelectedPatchResponse,
    signal,
    1024 * 1024,
  );
  const { file } = data;
  if (
    file.reviewId !== reviewId ||
    file.revision !== revision ||
    previous.revision !== revision ||
    (previous.settingsRevision != null && data.settingsRevision !== previous.settingsRevision) ||
    !Number.isSafeInteger(data.ordinal) ||
    data.ordinal < 1 ||
    ("path" in selection &&
      (file.oldPath !== selection.path.oldPath || file.newPath !== selection.path.newPath))
  ) {
    throw new Error("The selected file identity changed.");
  }
  let patch: RetainedPatch | undefined;
  if (data.text !== null) {
    const bytes = new TextEncoder().encode(data.text);
    if (
      !isRichPatch(file) ||
      bytes.length > RETAINED_PATCH_BYTES ||
      bytes.length !== file.byteLength
    )
      throw new Error("The selected patch length changed.");
    enforcePatchRows(bytes);
    const hash = await crypto.subtle.digest("SHA-256", bytes);
    const value = Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, "0"));
    const etag = `"${value.join("")}"`;
    if (file.patchETag !== etag) throw new Error("The selected patch digest changed.");
    signal.throwIfAborted();
    patch = {
      identity: patchIdentity(file, etag),
      result: { text: data.text, first: 0, next: bytes.length, total: bytes.length, etag },
    };
  }
  signal.throwIfAborted();
  const index: PatchIndexResult = {
    cursor,
    data: {
      ...previous,
      singleFile: true,
      pageNumber: data.ordinal,
      items: [file],
      previousCursor: data.previousCursor,
      nextCursor: data.nextCursor,
      settingsRevision: data.settingsRevision,
    },
  };
  return { index, patch };
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

export async function readPatch(
  file: PatchFile,
  signal: AbortSignal,
  window?: { readonly offset: number; readonly etag?: string; readonly last?: boolean },
): Promise<PatchWindow> {
  const headers = new Headers();
  const pinned = file.patchETag ?? window?.etag;
  const identity = patchIdentity(file, pinned);
  if (!window && pinned && retainedPatch?.identity === identity) {
    signal.throwIfAborted();
    return retainedPatch.result;
  }
  if (pinned) headers.set("If-Match", pinned);
  const offset = window?.offset ?? 0;
  const bounded = window === undefined ? MAX_RICH_BYTES : PATCH_WINDOW_BYTES;
  if (window)
    headers.set(
      "Range",
      window.last ? `bytes=-${bounded}` : `bytes=${offset}-${offset + bounded - 1}`,
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
    retainedPatch = bytes.length <= RETAINED_PATCH_BYTES ? { identity, result } : undefined;
    return result;
  }
  const [first, end, total] = contentRange(response);
  if (
    first !== (window.last ? Math.max(0, total - bounded) : offset) ||
    end !== Math.min(total, first + bounded) ||
    (file.byteLength !== null && total !== file.byteLength)
  ) {
    throw new Error("The selected patch range changed.");
  }
  const bytes = await readBoundedBytes(response, bounded);
  if (bytes.length !== end - first) throw new Error("The selected patch range was incomplete.");
  let leading = 0;
  while (leading < 3 && isContinuation(bytes[leading])) leading++;
  let next = bytes.length;
  if (end < total) {
    next--;
    while (next > leading && isContinuation(bytes[next])) next--;
    const firstByte = bytes[next] ?? 0;
    const width = firstByte < 0x80 ? 1 : firstByte < 0xe0 ? 2 : firstByte < 0xf0 ? 3 : 4;
    if (next + width <= bytes.length) next = bytes.length;
  }
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

function patchIdentity(file: PatchFile, etag: string | undefined) {
  return JSON.stringify([file.reviewId, file.revision, file.fileId, etag]);
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
