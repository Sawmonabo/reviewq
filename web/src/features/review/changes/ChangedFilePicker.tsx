import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { KeyboardEvent, ReactNode } from "react";
import { PATH_MATCHES_PER_PAGE, readPatchPathMatches } from "../../../api/changes";
import type { PatchIndex, readPatchPathCatalog } from "../../../api/changes";
import type { PatchFile, PatchPath, PatchSearchMatch } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { pathMatch, splitPath } from "./file-search";
import type { MatchRange } from "./file-search";
import { usePatchSearch } from "./usePatchSearch";

type Props = {
  readonly reviewId: string;
  readonly index: PatchIndex;
  readonly files: ReadonlyMap<number, PatchFile>;
  readonly selected: PatchFile | undefined;
  readonly position: number;
  readonly total: string | undefined;
  readonly pending: boolean;
  readonly catalog: {
    readonly data?: Awaited<ReturnType<typeof readPatchPathCatalog>>["data"] | null;
    readonly error?: string;
    readonly retry: () => void;
  };
  readonly navigation: {
    readonly previous: boolean;
    readonly next: boolean;
    readonly onStep: (direction: -1 | 1) => void;
  };
  readonly onChooseFile: (file: PatchFile) => void;
  readonly onChoosePath: (path: PatchPath) => void;
  readonly onChooseMatch: (match: PatchSearchMatch, query: string) => void;
};

type Entry = {
  readonly key: string;
  readonly path: string;
  readonly folder: string;
  readonly name: string;
  readonly cut: number;
  readonly marks: readonly MatchRange[];
  readonly selected: boolean;
  readonly file: PatchFile | undefined;
  readonly choose: () => void;
};

export function ChangedFilePicker({
  reviewId,
  index,
  files,
  selected,
  position,
  total,
  pending,
  catalog,
  navigation,
  onChooseFile,
  onChoosePath,
  onChooseMatch,
}: Props) {
  const id = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const results = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [search, setSearch] = useState("");
  const [visibleFileId, setVisibleFileId] = useState<string>();
  const [offset, setOffset] = useState(0);
  const [suggested, setSuggested] = useState<string>();
  const text = query.trim();
  const cached = catalog.data?.paths;
  const needsCatalogScan = Boolean(open && catalog.data && !cached);
  useEffect(() => {
    if (!open) return;
    const timer = window.setTimeout(() => setSearch(query), 180);
    return () => window.clearTimeout(timer);
  }, [open, query]);

  useEffect(() => {
    if (!open) return;
    const anchor = trigger.current;
    const element = dialog.current;
    if (!anchor || !element) return;
    function place() {
      if (!anchor || !element) return;
      const box = anchor.getBoundingClientRect();
      const header = anchor.closest(".changes-header")?.getBoundingClientRect();
      const width = Math.min(414, window.innerWidth - 24);
      const spaceBelow = window.innerHeight - box.bottom - 18;
      const above = spaceBelow < 260 && box.top > spaceBelow;
      element.style.setProperty(
        "--dialog-left",
        `${Math.min(window.innerWidth - width - 12, Math.max(12, box.right - width))}px`,
      );
      element.style.setProperty("--dialog-top", above ? "auto" : `${box.bottom + 6}px`);
      element.style.setProperty(
        "--dialog-bottom",
        above ? `${window.innerHeight - box.top + 6}px` : "auto",
      );
      element.style.setProperty(
        "--dialog-height",
        `${Math.min(window.innerHeight * 0.66, above ? box.top - 18 : spaceBelow)}px`,
      );
      element.style.setProperty("--dialog-attachment", `${(header?.bottom ?? box.bottom) + 6}px`);
    }
    const observer = new ResizeObserver(place);
    observer.observe(anchor);
    window.addEventListener("resize", place);
    const scroller = anchor.closest(".drawer-body");
    scroller?.addEventListener("scroll", place, { passive: true });
    place();
    element.showModal();
    input.current?.focus();
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", place);
      scroller?.removeEventListener("scroll", place);
      if (element.open) element.close();
    };
  }, [open]);

  const scanned = useResource(
    JSON.stringify(["file-search", reviewId, index.revision, needsCatalogScan, search, offset]),
    (signal) =>
      needsCatalogScan
        ? readPatchPathMatches(
            reviewId,
            index.revision,
            signal,
            (path) => Boolean(findPath(path, search.trim())),
            offset,
          )
        : Promise.resolve({ data: null, cursor: "" }),
  );
  const cachedMatches = useMemo(
    () => cached?.filter((path) => findPath(path, text)),
    [cached, text],
  );
  const found = cachedMatches
    ? {
        count: cachedMatches.length,
        paths: cachedMatches.slice(offset, offset + PATH_MATCHES_PER_PAGE),
      }
    : search.trim() === text
      ? scanned.data
      : undefined;
  const byPath = new Map([...files.values()].map((file) => [pathKey(file), file]));
  const entries: Entry[] = [];
  if (found) {
    for (const path of found.paths)
      addPath(path, path.lookupKey, byPath.get(pathKey(path)), () => onChoosePath(path));
  } else if (!cached) {
    for (const file of index.items) addPath(file, file.fileId, file, () => onChooseFile(file));
  }
  function addPath(
    path: Pick<PatchPath, "oldPath" | "newPath">,
    key: string,
    file: PatchFile | undefined,
    choose: () => void,
  ) {
    const matched = findPath(path, text);
    if (!matched) return;
    entries.push({
      key,
      ...matched,
      selected: Boolean(selected && pathKey(selected) === pathKey(path)),
      file,
      choose,
    });
  }
  const groups = new Map<string, Entry[]>();
  for (const entry of entries) {
    const group = groups.get(entry.folder);
    if (group) group.push(entry);
    else groups.set(entry.folder, [entry]);
  }
  const canSearch = Boolean(open && text && new TextEncoder().encode(query).length <= 256);
  const readingCode = canSearch && search === query;
  const code = usePatchSearch(reviewId, index.revision, readingCode ? search : "", visibleFileId);
  const matches = readingCode ? (code.data?.items ?? []) : [];
  const codeGroups = new Map<string, PatchSearchMatch[]>();
  for (const match of matches) {
    const path = match.side === "old" ? match.oldPath : match.newPath;
    const group = codeGroups.get(path);
    if (group) group.push(match);
    else codeGroups.set(path, [match]);
  }
  const firstMatch = matches[0];
  const active =
    suggested ??
    (!text ? entries.find((entry) => entry.selected)?.key : undefined) ??
    entries[0]?.key ??
    (firstMatch && matchKey(firstMatch));
  const count = found?.count ?? entries.length;
  const fileName = selected
    ? splitPath(selected.newPath).name
    : pending
      ? "Loading files…"
      : "Changed files";
  const ordinal = total ? `${position} of ${total}` : `File ${position}`;

  function close() {
    setOpen(false);
  }

  function choose(entry: Entry) {
    close();
    entry.choose();
  }

  function move(event: KeyboardEvent<HTMLDialogElement>) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp" && event.key !== "Enter") return;
    if (event.key === "Enter" && event.target !== input.current) return;
    const options = [
      ...(results.current?.querySelectorAll<HTMLButtonElement>("[data-option]") ?? []),
    ];
    const option = options.findIndex((item) => item.dataset.option === active);
    if (event.key === "Enter") {
      if (option >= 0) {
        event.preventDefault();
        options[option]?.click();
      }
      return;
    }
    const next =
      options[
        Math.max(0, Math.min(options.length - 1, option + (event.key === "ArrowDown" ? 1 : -1)))
      ];
    if (!next) return;
    event.preventDefault();
    setSuggested(next.dataset.option);
    if (event.target !== input.current) next.focus({ preventScroll: true });
    next.scrollIntoView({ block: "nearest" });
  }

  function page(next: number) {
    setSuggested(undefined);
    setOffset(next);
    results.current?.scrollTo(0, 0);
  }

  return (
    <nav
      className="changes-file-nav"
      aria-label="Changed file navigation"
      onKeyDown={(event) => {
        if (event.target instanceof Node && dialog.current?.contains(event.target)) return;
        if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
        event.preventDefault();
        navigation.onStep(event.key === "ArrowLeft" ? -1 : 1);
      }}
    >
      <button
        type="button"
        className="changes-picker-trigger"
        ref={trigger}
        aria-haspopup="dialog"
        aria-controls={id}
        aria-expanded={open}
        aria-label="Choose a changed file"
        aria-describedby={`${id}-position`}
        title={selected?.newPath}
        onClick={() => {
          setVisibleFileId(selected?.fileId);
          setQuery("");
          setSearch("");
          code.reset();
          setSuggested(undefined);
          setOffset(0);
          setOpen(true);
        }}
      >
        <Icon name="file" size={13} />
        <span className="changes-current-name" aria-hidden="true">
          {fileName}
        </span>
        <span className="changes-picker-position" aria-hidden="true">
          {ordinal}
          <Icon name="chevron" size={12} />
        </span>
      </button>
      <span id={`${id}-position`} hidden>
        {selected?.newPath} · {ordinal}
      </span>
      {(["Previous", "Next"] as const).map((label) => {
        const previous = label === "Previous";
        const available = previous ? navigation.previous : navigation.next;
        return (
          <button
            key={label}
            type="button"
            className="changes-file-step"
            aria-label={`${label} file`}
            aria-keyshortcuts={previous ? "ArrowLeft" : "ArrowRight"}
            aria-disabled={!available}
            title={`${label} file (${previous ? "←" : "→"} when focused here)`}
            onClick={() => available && navigation.onStep(previous ? -1 : 1)}
          >
            <Icon name={previous ? "chevronLeft" : "chevronRight"} size={14} />
          </button>
        );
      })}
      <dialog
        id={id}
        className="changes-picker-dialog"
        ref={dialog}
        aria-labelledby={`${id}-heading`}
        onClose={close}
        onKeyDown={move}
        onClick={(event) => {
          if (event.target === dialog.current) {
            const box = dialog.current.getBoundingClientRect();
            if (
              event.clientX < box.left ||
              event.clientX > box.right ||
              event.clientY < box.top ||
              event.clientY > box.bottom
            )
              close();
          }
        }}
      >
        <header className="changes-picker-heading">
          <h2 id={`${id}-heading`}>Changed files</h2>
          <button
            type="button"
            className="changes-picker-close"
            aria-label="Close changed files"
            title="Close (Escape)"
            onClick={close}
          >
            <Icon name="close" size={16} />
          </button>
        </header>
        <label className="changes-picker-search">
          <Icon name="search" size={14} />
          <input
            ref={input}
            aria-label="Search filenames or code in this diff"
            aria-controls={`${id}-results`}
            value={query}
            placeholder="Search filenames or code"
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => {
              setQuery(event.currentTarget.value);
              code.reset();
              page(0);
            }}
          />
        </label>
        <div
          ref={results}
          id={`${id}-results`}
          className="changes-picker-results"
          role="region"
          aria-label="Matching files and code"
        >
          {catalog.error && <Notice retry={catalog.retry}>{catalog.error}</Notice>}
          {scanned.error && <Notice retry={scanned.retry}>{scanned.error}</Notice>}
          {!!entries.length && (
            <div className="changes-picker-grid changes-picker-columns" aria-hidden="true">
              <span className="changes-picker-path">File / path</span>
              <span className="changes-picker-kind">Change</span>
              <span className="plus">+</span>
              <span className="minus">−</span>
            </div>
          )}
          {[...groups].map(([folder, items]) => (
            <div key={folder}>
              <p className="changes-picker-grid changes-picker-folder">
                <Icon name="folder" size={13} />
                <span className="changes-picker-path">
                  <Marked
                    text={folder || "Root"}
                    ranges={items[0]?.marks.filter(([start]) => start < folder.length) ?? []}
                  />
                </span>
              </p>
              <ul className="changes-picker-list">
                {items.map((entry) => (
                  <li key={entry.key}>
                    <button
                      type="button"
                      className="changes-picker-grid changes-picker-file"
                      data-option={entry.key}
                      data-suggested={entry.key === active || undefined}
                      aria-current={entry.selected || undefined}
                      title={entry.path}
                      aria-label={entry.path}
                      onFocus={() => setSuggested(entry.key)}
                      onClick={() => choose(entry)}
                    >
                      <Icon name="file" size={13} />
                      <code>
                        <Marked
                          text={entry.name}
                          ranges={entry.marks
                            .filter(([, end]) => end > entry.cut)
                            .map(([start, end]) => [
                              Math.max(0, start - entry.cut),
                              end - entry.cut,
                            ])}
                        />
                      </code>
                      {entry.file && (
                        <>
                          <span className="changes-picker-kind" data-kind={fileKind(entry.file)}>
                            {fileKind(entry.file)}
                          </span>
                          <DiffCount file={entry.file} />
                        </>
                      )}
                      <span className="changes-picker-check">
                        {entry.selected && <Icon name="check" size={13} />}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ))}
          {open && needsCatalogScan && (search.trim() !== text || scanned.pending) && (
            <Loading label="Searching changed files…" />
          )}
          {!!text && !entries.length && !matches.length && (found || catalog.error) && (
            <p className="changes-picker-empty">No matching files.</p>
          )}
          {!!text && (
            <>
              <p className="changes-code-heading">Code in this diff</p>
              {[...codeGroups].map(([path, hits]) => (
                <div key={path}>
                  <p className="changes-code-file">{path}</p>
                  <ul className="changes-picker-list">
                    {hits.map((hit) => {
                      const side = hit.side === "old" ? "Old" : "New";
                      const key = matchKey(hit);
                      return (
                        <li key={key}>
                          <button
                            type="button"
                            className="changes-code-choice"
                            data-option={key}
                            data-suggested={key === active || undefined}
                            aria-label={`${path}, ${side} ${hit.line}, ${hit.snippet}`}
                            onFocus={() => setSuggested(key)}
                            onClick={() => {
                              close();
                              onChooseMatch(hit, search);
                            }}
                          >
                            <span className="changes-code-location">
                              {side} {hit.line}
                            </span>
                            <code>
                              <Marked
                                text={hit.snippet}
                                ranges={[[hit.highlightStart, hit.highlightEnd]]}
                              />
                            </code>
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                </div>
              ))}
              {!canSearch && (
                <p className="changes-picker-empty">Code queries can be up to 256 bytes.</p>
              )}
              {canSearch && (!readingCode || code.pending) && (
                <Loading label="Searching diff code…" />
              )}
              {readingCode && code.error && <Notice retry={code.retry}>{code.error}</Notice>}
              {readingCode && code.data?.coverage === "limited" && (
                <p className="changes-picker-empty">
                  Search coverage is limited by the available GitLab patch.
                </p>
              )}
              {readingCode && code.data?.coverage === "complete" && !matches.length && (
                <p className="changes-picker-empty">No matching code.</p>
              )}
              {readingCode && code.data?.nextCursor && (
                <button
                  type="button"
                  className="changes-clear"
                  onClick={() => {
                    setSuggested(undefined);
                    code.more();
                  }}
                >
                  {matches.length ? "More code matches" : "Continue searching code"}
                </button>
              )}
            </>
          )}
        </div>
        {(offset > 0 || count > PATH_MATCHES_PER_PAGE) && (
          <nav className="pagination" aria-label="File search pages">
            <button
              type="button"
              disabled={offset === 0}
              onClick={() => page(offset - PATH_MATCHES_PER_PAGE)}
            >
              Previous
            </button>
            <span className="meta">
              {offset + 1}–{offset + entries.length} of {count}
            </span>
            <button
              type="button"
              disabled={offset + PATH_MATCHES_PER_PAGE >= count}
              onClick={() => page(offset + PATH_MATCHES_PER_PAGE)}
            >
              Next
            </button>
          </nav>
        )}
        <footer className="changes-picker-footer">
          <span role="status">
            {count} {count === 1 ? "file" : "files"}
            {text &&
              ` · ${matches.length} code ${matches.length === 1 ? "match" : "matches"}${code.data?.nextCursor ? "+" : ""}`}
            {catalog.data?.partial && " · Partial GitLab file list"}
          </span>
          <span>
            <kbd>↑</kbd>
            <kbd>↓</kbd> Move <kbd>Enter</kbd> Jump
          </span>
        </footer>
      </dialog>
    </nav>
  );
}

function Marked({
  text,
  ranges,
}: {
  readonly text: string;
  readonly ranges: readonly MatchRange[];
}) {
  let end = 0;
  const pieces: ReactNode[] = [];
  for (const [start, finish] of ranges) {
    const from = Math.max(start, end);
    if (finish <= from) continue;
    pieces.push(text.slice(end, from), <mark key={from}>{text.slice(from, finish)}</mark>);
    end = finish;
  }
  return (
    <>
      {pieces}
      {text.slice(end)}
    </>
  );
}

function matchKey(match: PatchSearchMatch) {
  return `match:${match.fileId}:${match.sourceStart}:${match.sourceEnd}`;
}

function findPath(path: Pick<PatchPath, "oldPath" | "newPath">, text: string) {
  for (const candidate of new Set([path.newPath, path.oldPath])) {
    const marks = pathMatch(candidate, text);
    if (marks) return { path: candidate, ...splitPath(candidate), marks };
  }
}

function pathKey({ oldPath, newPath }: Pick<PatchPath, "oldPath" | "newPath">) {
  return JSON.stringify([oldPath, newPath]);
}

export function DiffCount({ file }: { readonly file: PatchFile }) {
  const { additions, deletions } = file;
  if (additions == null || deletions == null) return null;
  return (
    <small className="diff-count" aria-label={`${additions} additions, ${deletions} deletions`}>
      <span className="plus">+{additions}</span> <span className="minus">−{deletions}</span>
    </small>
  );
}

export function FileKind({ file }: { readonly file: PatchFile }) {
  return fileKind(file);
}

function fileKind(file: PatchFile) {
  return file.deleted ? "Deleted" : file.created ? "Added" : file.renamed ? "Renamed" : "Modified";
}
