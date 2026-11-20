import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import {
  useCallback,
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { CSSProperties } from "react";
import { isRichPatch, PATCH_WINDOW_BYTES } from "../../../api/changes";
import type { PatchIndex, readPatchPathCatalog } from "../../../api/changes";
import type { PatchDiscussion, PatchFile, PatchPath } from "../../../api/generated/contracts";
import { Empty, Notice } from "../../../components/PanelState";
import { safeUrl } from "../../../lib/presentation";
import { ChangedFilePicker } from "./ChangedFilePicker";
import { FilePatch } from "./FilePatch";
import {
  alignPatch,
  lineAt,
  matchingFile,
  storedFile,
  storedPatch,
  storePatch,
} from "./patch-position";
import type { PatchLine } from "./patch-position";
import { pageAt, pageStart, usePatchPages } from "./usePatchPages";

type Props = {
  readonly reviewId: string;
  readonly initial: PatchIndex;
  readonly catalog: {
    readonly data?: Awaited<ReturnType<typeof readPatchPathCatalog>>["data"] | null;
    readonly error?: string;
    readonly retry: () => void;
  };
  readonly threads: readonly PatchDiscussion[];
  readonly webUrl: string;
  readonly query: string;
  readonly onQuery: (value: string) => void;
  readonly onLoadedAnchors: (threads: ReadonlySet<string> | undefined) => void;
  readonly restore: boolean;
  readonly onReload: () => void;
};

type Jump = {
  readonly index: number;
  readonly inset: number;
  readonly focus: boolean;
  readonly line?: PatchLine;
};
const MAX_ACTIVE_PATCHES = 8;
const MAX_ACTIVE_BYTES = 4 * 1024 * 1024;
const MAX_AHEAD_BYTES = 256 * 1024;
const EMPTY_THREADS: readonly PatchDiscussion[] = [];

export function FileStream({
  reviewId,
  initial,
  catalog,
  threads,
  webUrl,
  query,
  onQuery,
  onLoadedAnchors,
  restore,
  onReload,
}: Props) {
  const toolbar = useRef<HTMLDivElement>(null);
  const [host, setHost] = useState<HTMLDivElement | null>(null);
  const [geometry, setGeometry] = useState({ margin: 0, toolbar: 0 });
  const [pinned, setPinned] = useState<readonly number[]>([]);
  const index = usePatchPages(reviewId, initial, pinned);
  const releaseAlignment = useRef<(() => void) | undefined>(undefined);
  const [landed, setLanded] = useState<{ readonly index: number; readonly offset: number }>();
  useEffect(() => () => releaseAlignment.current?.(), []);
  const [jump, setJump] = useState<Jump | undefined>(() => {
    if (!restore) return;
    const saved = storedPatch(reviewId, initial.revision);
    const file = initial.items.findIndex(
      (entry) => entry.fileId === (saved?.fileId ?? storedFile(reviewId)),
    );
    if (file < 0 || (!saved?.inset && !saved?.line && initial.pageNumber === 1 && file === 0))
      return;
    return {
      index: pageStart(initial.pageNumber) + file,
      inset: saved?.inset ?? 0,
      focus: false,
      ...(saved?.line && { line: saved.line }),
    };
  });
  const scroller = host?.closest<HTMLElement>(".drawer-body") ?? null;
  const end = pageStart(index.tail.pageNumber) + index.tail.length;
  const count = index.tail.nextCursor
    ? Math.max(end, pageStart(index.tail.pageNumber + 1)) + 1
    : end;
  const source = safeUrl(webUrl);
  const total = catalog.data
    ? `${Math.max(end, catalog.data.count)}${catalog.data.partial ? "+" : ""}`
    : index.tail.nextCursor
      ? undefined
      : String(end);
  const files = useMemo(() => {
    const entries = new Map<number, PatchFile>();
    for (const page of index.pages.values())
      page.items.forEach((file, local) => entries.set(pageStart(page.pageNumber) + local, file));
    return entries;
  }, [index.pages]);
  const anchors = useMemo(() => {
    const entries = new Map<number, readonly PatchDiscussion[]>();
    for (const [number, file] of files) {
      const matched = threads.filter((thread) => matchingFile(thread, file));
      if (matched.length) entries.set(number, matched);
    }
    return entries;
  }, [files, threads]);
  useEffect(() => {
    const ids = new Set<string>();
    for (const matched of anchors.values())
      for (const thread of matched) ids.add(thread.discussionId);
    onLoadedAnchors(ids);
  }, [anchors, onLoadedAnchors]);
  useEffect(() => () => onLoadedAnchors(undefined), [onLoadedAnchors]);

  const rangeExtractor = useCallback(
    (range: Parameters<typeof defaultRangeExtractor>[0]) => {
      const items = new Set([
        ...defaultRangeExtractor(range),
        ...pinned.filter((ordinal) => ordinal >= 0 && ordinal < range.count),
      ]);
      if (jump && jump.index < range.count) items.add(jump.index);
      return [...items].sort((left, right) => left - right);
    },
    [jump, pinned],
  );

  // File ordinals remain stable when a distant page arrives or drops out.
  // eslint-disable-next-line react-hooks/incompatible-library
  const list = useVirtualizer({
    count,
    getScrollElement: () => scroller,
    estimateSize: (ordinal) => estimateFile(files.get(ordinal)),
    overscan: 4,
    rangeExtractor,
    scrollMargin: geometry.margin,
    scrollPaddingStart: geometry.toolbar,
  });
  useLayoutEffect(() => {
    list.shouldAdjustScrollPositionOnItemSizeChange = (item, _delta, virtualizer) =>
      item.end <= (virtualizer.scrollOffset ?? 0) + geometry.toolbar;
  }, [geometry.toolbar, list]);
  const offset = list.scrollOffset ?? 0;
  const leading = list.getVirtualItemForOffset(Math.max(0, offset + geometry.toolbar));
  const viewportFirst = Math.max(
    0,
    leading?.index ?? list.range?.startIndex ?? jump?.index ?? pageStart(initial.pageNumber),
  );
  const last = Math.max(viewportFirst, list.range?.endIndex ?? viewportFirst);
  const first =
    landed && Math.abs(landed.offset - offset) <= 1 && files.has(landed.index)
      ? landed.index
      : viewportFirst;
  const active = files.get(first);
  const shown = index.pages.get(pageAt(first)) ?? initial;
  const virtual = list.getVirtualItems();
  const backwards = list.scrollDirection === "backward";
  const nearLarge = backwards
    ? list.getVirtualItemForOffset(Math.max(0, offset - 800))?.index
    : list.getVirtualItemForOffset(offset + (list.scrollRect?.height ?? 0) + 800)?.index;
  const warmLarge =
    nearLarge !== undefined && (backwards ? nearLarge < viewportFirst : nearLarge > last)
      ? backwards
        ? viewportFirst - 1
        : last + 1
      : undefined;
  const wanted = useMemo(
    () =>
      admitPatches(files, viewportFirst, last, backwards, pinned, jump?.index ?? first, warmLarge),
    [files, viewportFirst, last, backwards, pinned, jump?.index, first, warmLarge],
  );

  useLayoutEffect(() => {
    if (!host || !scroller || !toolbar.current) return;
    const element = host;
    const scroll = scroller;
    const controls = toolbar.current;
    function measure() {
      const margin =
        scroll.scrollTop + element.getBoundingClientRect().top - scroll.getBoundingClientRect().top;
      const height = controls.getBoundingClientRect().height;
      setGeometry((current) =>
        current.margin === margin && current.toolbar === height
          ? current
          : { margin, toolbar: height },
      );
    }
    const observer = new ResizeObserver(measure);
    observer.observe(scroll);
    observer.observe(controls);
    if (element.parentElement) observer.observe(element.parentElement);
    measure();
    return () => observer.disconnect();
  }, [host, scroller]);

  useLayoutEffect(() => {
    if (!jump || !host || !scroller || !geometry.margin || !files.has(jump.index)) return;
    const scroll = scroller;
    const element = host;
    releaseAlignment.current?.();
    list.scrollToIndex(jump.index, { align: "start" });
    const frame = requestAnimationFrame(() => {
      const file = element.querySelector<HTMLElement>(`[data-file-position="${jump.index + 1}"]`);
      if (!file) return;
      list.cancelScroll();
      releaseAlignment.current = alignPatch(
        file,
        scroll,
        geometry.toolbar,
        jump.inset,
        jump.line,
        () => {
          if (jump.focus)
            file.querySelector<HTMLElement>(".patch-file-title")?.focus({ preventScroll: true });
          setLanded({ index: jump.index, offset: scroll.scrollTop });
          setJump(undefined);
        },
      );
    });
    return () => cancelAnimationFrame(frame);
  }, [files, geometry.margin, geometry.toolbar, host, jump, list, scroller]);

  const remember = useEffectEvent(() => {
    if (
      !active ||
      !scroller ||
      !host?.isConnected ||
      jump ||
      scroller.scrollTop + geometry.toolbar < geometry.margin - 2
    )
      return;
    const item = host.querySelector<HTMLElement>(`[data-file-position="${first + 1}"]`);
    if (item && !item.querySelector(".patch-waiting")) {
      const inset =
        scroller.getBoundingClientRect().top + geometry.toolbar - item.getBoundingClientRect().top;
      const line = lineAt(item, scroller, geometry.toolbar);
      storePatch(reviewId, initial.revision, {
        fileId: active.fileId,
        inset: Math.max(0, inset),
        ...(line && { line }),
      });
    }
  });

  useEffect(() => {
    if (!scroller || !host) return;
    const element = host;
    const scroll = scroller;
    let timer: number | undefined;
    function record() {
      window.clearTimeout(timer);
      timer = window.setTimeout(remember, 160);
    }
    function keepLiveNodes() {
      const selection = getSelection();
      const start = fileIndex(selection?.anchorNode);
      const end = fileIndex(selection?.focusNode);
      const focus = fileIndex(document.activeElement);
      const held = new Set<number>();
      if (focus !== undefined && element.contains(document.activeElement)) held.add(focus);
      if (
        selection &&
        !selection.isCollapsed &&
        start !== undefined &&
        end !== undefined &&
        element.contains(selection.anchorNode) &&
        element.contains(selection.focusNode)
      ) {
        const low = Math.min(start, end);
        const high = Math.max(start, end);
        for (const row of element.querySelectorAll("[data-file-position]")) {
          const ordinal = fileIndex(row);
          if (ordinal !== undefined && ordinal >= low && ordinal <= high) held.add(ordinal);
        }
      }
      setPinned((current) =>
        current.length === held.size && current.every((item) => held.has(item))
          ? current
          : [...held],
      );
    }
    scroll.addEventListener("scroll", record, { passive: true });
    window.addEventListener("pagehide", remember);
    document.addEventListener("selectionchange", keepLiveNodes);
    document.addEventListener("focusin", keepLiveNodes);
    return () => {
      window.clearTimeout(timer);
      remember();
      scroll.removeEventListener("scroll", record);
      window.removeEventListener("pagehide", remember);
      document.removeEventListener("selectionchange", keepLiveNodes);
      document.removeEventListener("focusin", keepLiveNodes);
    };
  }, [host, scroller]);

  const fetchNearby = useEffectEvent(() => {
    if (index.pending || index.error || !scroller || !count) return;
    const start = jump?.index ?? viewportFirst;
    const finish = jump?.index ?? last;
    const before = pageStart(pageAt(start)) - 1;
    const after = pageStart(pageAt(finish) + 1);
    const wantedPositions = backwards
      ? [start, finish, before, after]
      : [start, finish, after, before];
    for (const position of wantedPositions) {
      if (position < 0 || position >= count || index.pages.has(pageAt(position))) continue;
      const target = index.lookupPage(pageAt(position));
      if (target) {
        void index.load(target, false);
        break;
      }
    }
  });
  useEffect(() => {
    fetchNearby();
  }, [
    backwards,
    count,
    viewportFirst,
    index.error,
    index.pages,
    index.pending,
    jump,
    last,
    scroller,
  ]);

  function goTo(ordinal: number, focus = true) {
    index.cancelManual();
    toolbar.current?.querySelector("details")?.removeAttribute("open");
    setJump({ index: ordinal, inset: 0, focus });
  }

  function chooseFile(file: PatchFile) {
    for (const [ordinal, entry] of files)
      if (entry.fileId === file.fileId) {
        goTo(ordinal);
        return;
      }
  }

  async function choosePath(path: PatchPath) {
    for (const [ordinal, file] of files)
      if (file.oldPath === path.oldPath && file.newPath === path.newPath) {
        goTo(ordinal);
        return;
      }
    const page = await index.load({ path }, true);
    if (page) {
      const offset = page.items.findIndex(
        (file) => file.oldPath === path.oldPath && file.newPath === path.newPath,
      );
      if (offset >= 0) goTo(pageStart(page.pageNumber) + offset);
    }
  }

  async function goToOrdinal(ordinal: number) {
    if (ordinal < 0 || ordinal >= count) return;
    if (files.has(ordinal)) {
      goTo(ordinal, false);
      return;
    }
    const target = index.lookupPage(pageAt(ordinal));
    if (target && target.page === pageAt(ordinal)) {
      const page = await index.load(target, true);
      if (page?.items[ordinal - pageStart(page.pageNumber)]) goTo(ordinal, false);
    }
  }

  return (
    <div className="file-stream">
      <div className="file-stream-controls" ref={toolbar}>
        <ChangedFilePicker
          reviewId={reviewId}
          index={shown}
          selected={active}
          total={total}
          pending={index.pending && !active}
          catalog={catalog}
          query={query}
          onQuery={onQuery}
          onChooseFile={chooseFile}
          onChoosePath={(path) => void choosePath(path)}
          onPage={(cursor) => {
            const page = shown.pageNumber + (cursor === shown.previousCursor ? -1 : 1);
            if (cursor)
              void index.load({ page, cursor }, true).then((result) => {
                if (result?.items.length) goTo(pageStart(result.pageNumber));
              });
          }}
        />
        {count > 0 && (
          <div className="actions patch-arrows" aria-label="Changed file position">
            <button
              type="button"
              aria-label="Previous file"
              aria-disabled={first === 0}
              onClick={() => void goToOrdinal(first - 1)}
            >
              ‹
            </button>
            <small>{total === undefined ? `File ${first + 1}` : `${first + 1}/${total}`}</small>
            <button
              type="button"
              aria-label="Next file"
              aria-disabled={first + 1 >= count}
              onClick={() => void goToOrdinal(first + 1)}
            >
              ›
            </button>
          </div>
        )}
      </div>
      {index.error && (
        <Notice retry={index.retry}>
          {index.error}{" "}
          <button type="button" onClick={onReload}>
            Reload file index
          </button>
        </Notice>
      )}
      {!count && <Empty title="No changed files" />}
      <div
        className="file-stream-list"
        ref={setHost}
        style={{ height: list.getTotalSize() }}
        aria-label="File diffs"
      >
        {virtual.map((item) => {
          const file = files.get(item.index);
          return (
            <div
              className="file-stream-slot"
              key={item.key}
              data-index={item.index}
              ref={list.measureElement}
              style={
                {
                  transform: `translateY(${item.start - geometry.margin}px)`,
                  minHeight: file ? undefined : item.size,
                  "--file-estimate": `${file ? Math.max(item.size, estimateFile(file)) : item.size}px`,
                } as CSSProperties
              }
            >
              {file ? (
                <FilePatch
                  key={file.fileId}
                  file={file}
                  threads={anchors.get(item.index) ?? EMPTY_THREADS}
                  source={source}
                  position={item.index + 1}
                  admitted={wanted.has(item.index)}
                  priorityLine={item.index === jump?.index ? jump.line : undefined}
                  visible={
                    (item.index >= viewportFirst && item.index <= last) ||
                    item.index === jump?.index
                  }
                />
              ) : (
                item.index >= viewportFirst &&
                item.index <= last &&
                !index.error && (
                  <p className="meta file-stream-loading" role="status">
                    Loading changed files…
                  </p>
                )
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function fileIndex(node: Node | null | undefined) {
  const element = node instanceof Element ? node : node?.parentElement;
  const position = element?.closest<HTMLElement>("[data-file-position]")?.dataset.filePosition;
  return position === undefined ? undefined : Number(position) - 1;
}

function estimateFile(file: PatchFile | undefined) {
  return 95 + (file && isRichPatch(file) ? Math.max(1, file.lineCount ?? 1) * 21 : 60);
}

function admitPatches(
  files: ReadonlyMap<number, PatchFile>,
  first: number,
  last: number,
  backwards: boolean,
  pinned: readonly number[],
  jump: number | undefined,
  warmLarge: number | undefined,
) {
  const visible = Array.from(
    { length: Math.min(last - first + 1, MAX_ACTIVE_PATCHES) },
    (_, n) => first + n,
  );
  const ahead = backwards ? [first - 1, first - 2, last + 1] : [last + 1, last + 2, first - 1];
  const candidates = [...pinned, ...(jump === undefined ? [] : [jump]), ...visible, ...ahead];
  const admitted = new Set<number>();
  let bytes = 0;
  for (const ordinal of candidates) {
    if (admitted.size >= MAX_ACTIVE_PATCHES) break;
    const file = files.get(ordinal);
    if (!file || admitted.has(ordinal) || file.availability !== "complete") continue;
    const size = isRichPatch(file) ? (file.byteLength ?? MAX_ACTIVE_BYTES) : PATCH_WINDOW_BYTES;
    if (bytes + size > MAX_ACTIVE_BYTES) continue;
    if (
      !(ordinal >= first && ordinal <= last) &&
      ordinal !== jump &&
      ordinal !== warmLarge &&
      !pinned.includes(ordinal) &&
      size > MAX_AHEAD_BYTES
    )
      continue;
    admitted.add(ordinal);
    bytes += size;
  }
  return admitted;
}
