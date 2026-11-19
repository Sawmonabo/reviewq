import { useEffect, useMemo, useRef, useState } from "react";
import {
  matchesPatchPath,
  PATH_MATCHES_PER_PAGE,
  readPatchPathMatches,
} from "../../../api/changes";
import type { readPatchPathCatalog } from "../../../api/changes";
import type { PatchFile, PatchFileIndex, PatchPath } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";

type Props = {
  readonly reviewId: string;
  readonly index: PatchFileIndex;
  readonly selected: PatchFile | undefined;
  readonly total: string | undefined;
  readonly pending: boolean;
  readonly query: string;
  readonly catalog: {
    readonly data?: Awaited<ReturnType<typeof readPatchPathCatalog>>["data"] | null;
    readonly error?: string;
    readonly retry: () => void;
  };
  readonly onChooseFile: (file: PatchFile) => void;
  readonly onQuery: (value: string) => void;
  readonly onChoosePath: (path: PatchPath) => void;
  readonly onPage: (cursor: string | null) => void;
};

export function ChangedFilePicker({
  reviewId,
  index,
  selected,
  total,
  pending,
  query,
  catalog,
  onQuery,
  onChooseFile,
  onChoosePath,
  onPage,
}: Props) {
  const input = useRef<HTMLInputElement>(null);
  const [search, setSearch] = useState("");
  const [knownCount, setKnownCount] = useState<{ readonly text: string; readonly count: number }>();
  const [offset, setOffset] = useState(0);
  const text = query.toLowerCase();
  const cached = catalog.data?.paths;
  const needsSearch = Boolean(catalog.data && !cached && text);
  useEffect(() => {
    if (!needsSearch) return;
    const timer = window.setTimeout(() => setSearch(text), 180);
    return () => window.clearTimeout(timer);
  }, [needsSearch, text]);
  const readingSearch = needsSearch && search === text;
  const matches = useResource(
    JSON.stringify(["file-search", reviewId, index.revision, readingSearch, search, offset]),
    async (signal) => {
      if (!readingSearch) return { data: null, cursor: "" };
      const result = await readPatchPathMatches(reviewId, index.revision, signal, search, offset);
      if (!signal.aborted) setKnownCount({ text: search, count: result.data.count });
      return result;
    },
  );
  const cachedMatches = useMemo(
    () => (text ? cached?.filter((path) => matchesPatchPath(path, text)) : undefined),
    [cached, text],
  );
  const result = cachedMatches
    ? {
        count: cachedMatches.length,
        paths: cachedMatches.slice(offset, offset + PATH_MATCHES_PER_PAGE),
      }
    : search === text
      ? matches.data
      : undefined;
  const searching = Boolean(text && !result && !catalog.error && !matches.error);
  const count = result?.count ?? (knownCount?.text === text ? knownCount.count : 0);

  return (
    <details
      className="changed-file-picker"
      onToggle={(event) => {
        if (event.currentTarget.open) input.current?.focus();
      }}
    >
      <summary>
        Changed files <small>{total}</small>
      </summary>
      <label className="search-field">
        <Icon name="search" size={13} />
        <input
          ref={input}
          aria-label="Search changed files"
          value={query}
          placeholder="Find a file"
          onChange={(event) => {
            onQuery(event.currentTarget.value);
            setOffset(0);
          }}
        />
      </label>
      {text && (catalog.error || matches.error) && (
        <Notice retry={catalog.error ? catalog.retry : matches.retry}>
          {catalog.error ?? matches.error}
        </Notice>
      )}
      <nav aria-label="Changed files" aria-busy={searching || pending}>
        {text ? (
          <>
            {searching && <Loading label="Searching changed files…" />}
            {result?.paths.map((path) => (
              <button
                type="button"
                key={path.lookupKey}
                aria-current={
                  (path.oldPath === selected?.oldPath && path.newPath === selected.newPath) ||
                  undefined
                }
                aria-disabled={pending}
                onClick={() => onChoosePath(path)}
              >
                <code>{pathLabel(path)}</code>
              </button>
            ))}
            {result && !result.count && <p className="meta">No matching files.</p>}
            {catalog.data?.partial && <p className="meta">GitLab supplied a partial file list.</p>}
          </>
        ) : (
          <>
            {index.items.map((item) => (
              <button
                type="button"
                key={item.fileId}
                aria-current={item.fileId === selected?.fileId || undefined}
                aria-disabled={pending}
                onClick={() => onChooseFile(item)}
              >
                <FileKind file={item} abbreviated />
                <code>{pathLabel(item)}</code>
                <DiffCount file={item} />
              </button>
            ))}
            {!index.items.length && <p className="meta">No changed files.</p>}
          </>
        )}
      </nav>
      {text
        ? (offset > 0 || count > PATH_MATCHES_PER_PAGE) && (
            <nav className="pagination" aria-label="File search pages">
              <button
                type="button"
                aria-disabled={searching || offset === 0}
                onClick={() =>
                  !searching && offset > 0 && setOffset(offset - PATH_MATCHES_PER_PAGE)
                }
              >
                Previous
              </button>
              <span className="meta" role="status">
                {result
                  ? `${offset + 1}–${offset + result.paths.length} of ${count}`
                  : searching
                    ? "Loading files…"
                    : null}
              </span>
              <button
                type="button"
                aria-disabled={searching || offset + PATH_MATCHES_PER_PAGE >= count}
                onClick={() =>
                  !searching &&
                  offset + PATH_MATCHES_PER_PAGE < count &&
                  setOffset(offset + PATH_MATCHES_PER_PAGE)
                }
              >
                Next
              </button>
            </nav>
          )
        : (index.previousCursor || index.nextCursor) && (
            <nav className="pagination" aria-label="Changed file pages">
              <button
                type="button"
                aria-disabled={!index.previousCursor || pending}
                onClick={() => onPage(index.previousCursor)}
              >
                Previous
              </button>
              <span className="meta" role="status">
                {pending ? "Loading files…" : `Page ${index.pageNumber}`}
              </span>
              <button
                type="button"
                aria-disabled={!index.nextCursor || pending}
                onClick={() => onPage(index.nextCursor)}
              >
                Next
              </button>
            </nav>
          )}
    </details>
  );
}

export function DiffCount({ file }: { readonly file: PatchFile }) {
  const { additions, deletions } = file;
  if (additions == null || deletions == null) return null;
  return (
    <small className="diff-count" aria-label={`${additions} additions, ${deletions} deletions`}>
      <span>+{additions}</span> <span>−{deletions}</span>
    </small>
  );
}

export function FileKind({
  file,
  abbreviated,
}: {
  readonly file: PatchFile;
  readonly abbreviated?: boolean;
}) {
  const kind = file.deleted
    ? "Deleted"
    : file.created
      ? "Added"
      : file.renamed
        ? "Renamed"
        : "Modified";
  return abbreviated ? (
    <span className="file-initial" aria-label={kind}>
      {kind.charAt(0)}
    </span>
  ) : (
    kind
  );
}

function pathLabel({ oldPath, newPath }: Pick<PatchPath, "oldPath" | "newPath">) {
  return oldPath === newPath ? newPath : `${oldPath} → ${newPath}`;
}
