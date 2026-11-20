import { useEffect, useMemo, useRef, useState } from "react";
import {
  matchesPatchPath,
  PATH_MATCHES_PER_PAGE,
  readPatchPathMatches,
} from "../../../api/changes";
import type { PatchIndex, readPatchPathCatalog } from "../../../api/changes";
import type { PatchFile, PatchPath } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";

type Props = {
  readonly reviewId: string;
  readonly index: PatchIndex;
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
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [knownCount, setKnownCount] = useState<{ readonly text: string; readonly count: number }>();
  const [pageOffset, setPageOffset] = useState<number>();
  const text = query.toLowerCase();
  const cached = catalog.data?.paths;
  const useCatalog = Boolean(text);
  const offset = pageOffset ?? 0;
  const needsSearch = Boolean(open && catalog.data && !cached && useCatalog);
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
    () => (text ? cached?.filter((path) => matchesPatchPath(path, text)) : cached),
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
  const searching = Boolean(useCatalog && !result && !catalog.error && !matches.error);
  const count = result?.count ?? (knownCount?.text === text ? knownCount.count : 0);
  return (
    <details
      className="changed-file-picker"
      onToggle={(event) => {
        const isOpen = event.currentTarget.open;
        setOpen(isOpen);
        if (isOpen) input.current?.focus();
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
            setPageOffset(undefined);
          }}
        />
      </label>
      {useCatalog && (catalog.error || matches.error) && (
        <Notice retry={catalog.error ? catalog.retry : matches.retry}>
          {catalog.error ?? matches.error}
        </Notice>
      )}
      <nav aria-label="Changed files" aria-busy={searching || pending}>
        {useCatalog ? (
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
                {selected &&
                  path.oldPath === selected.oldPath &&
                  path.newPath === selected.newPath && <FileKind file={selected} abbreviated />}
                <code>{pathLabel(path)}</code>
                {selected &&
                  path.oldPath === selected.oldPath &&
                  path.newPath === selected.newPath && <DiffCount file={selected} />}
              </button>
            ))}
            {result && !result.count && (
              <p className="meta">{text ? "No matching files." : "No changed files."}</p>
            )}
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
      {catalog.data?.partial && (
        <p className="meta file-picker-coverage">GitLab supplied a partial file list.</p>
      )}
      {useCatalog
        ? (offset > 0 || count > PATH_MATCHES_PER_PAGE) && (
            <nav
              className="pagination"
              aria-label={text ? "File search pages" : "Changed file pages"}
            >
              <button
                type="button"
                aria-disabled={searching || offset === 0}
                onClick={() =>
                  !searching && offset > 0 && setPageOffset(offset - PATH_MATCHES_PER_PAGE)
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
                  setPageOffset(offset + PATH_MATCHES_PER_PAGE)
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
