import { decodeUtf8, readBoundedBytes } from "./bounded-bytes";
import { read, request } from "./client";
import type { PatchDiscussions, PatchFile, PatchFileIndex } from "./generated/contracts";
import { isPatchDiscussionsResponse, isPatchFileIndexResponse } from "./generated/validators.mjs";

export const PATCH_WINDOW_BYTES = 64 * 1024;
const MAX_RICH_BYTES = 2 * 1024 * 1024;

export async function readPatchFiles(
  reviewId: string,
  signal: AbortSignal,
  position?: { readonly revision: string; readonly cursor: string },
  preferredFileId?: string,
): Promise<PatchFileIndex> {
  const query = new URLSearchParams();
  if (position) {
    query.set("revision", position.revision);
    query.set("cursor", position.cursor);
  }
  if (preferredFileId) query.set("preferredFileId", preferredFileId);
  const { data } = await read(
    `${changesRoute(reviewId)}/files?${query}`,
    isPatchFileIndexResponse,
    signal,
  );
  if (
    (position && data.revision !== position.revision) ||
    data.items.some((item) => item.reviewId !== reviewId || item.revision !== data.revision)
  ) {
    throw new Error("The selected patch index changed.");
  }
  return data;
}

export async function readPatchDiscussions(
  reviewId: string,
  revision: string,
  signal: AbortSignal,
  cursor?: string,
): Promise<PatchDiscussions> {
  const query = new URLSearchParams({ revision });
  if (cursor) query.set("cursor", cursor);
  const { data } = await read(
    `${changesRoute(reviewId)}/discussions?${query}`,
    isPatchDiscussionsResponse,
    signal,
  );
  if (data.reviewId !== reviewId || data.revision !== revision) {
    throw new Error("The selected discussion scope changed.");
  }
  return data;
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
  window?: { readonly offset: number; readonly etag?: string },
): Promise<PatchWindow> {
  const headers = new Headers();
  const pinned = file.patchETag ?? window?.etag;
  if (pinned) headers.set("If-Match", pinned);
  const offset = window?.offset ?? 0;
  const bounded = window === undefined ? MAX_RICH_BYTES : PATCH_WINDOW_BYTES + 3;
  if (window) headers.set("Range", `bytes=${offset}-${offset + bounded - 1}`);
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
    throw new Error("The response did not prove the selected patch identity.");
  }
  if (!window) {
    const bytes = await readBoundedBytes(response, bounded);
    if (bytes.length !== file.byteLength) throw new Error("The selected patch length changed.");
    return { text: decodeUtf8(bytes), first: 0, next: bytes.length, total: bytes.length, etag };
  }
  const [first, end, total] = contentRange(response);
  if (
    first !== offset ||
    end !== Math.min(total, offset + bounded) ||
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
    next = Math.min(bytes.length, PATCH_WINDOW_BYTES);
    while (isContinuation(bytes[next])) next++;
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
