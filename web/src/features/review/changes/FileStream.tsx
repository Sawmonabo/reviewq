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
import type { CSSProperties, RefObject } from "react";
import { createPortal } from "react-dom";
import { isRichPatch, PATCH_WINDOW_BYTES } from "../../../api/changes";
import type { PatchIndex, readPatchPathCatalog } from "../../../api/changes";
import type {
  PatchDiscussion,
  PatchFile,
  PatchPath,
  PatchSearchMatch,
} from "../../../api/generated/contracts";
import { Empty, Notice } from "../../../components/PanelState";
import { safeUrl } from "../../../lib/presentation";
import { ChangedFilePicker } from "./ChangedFilePicker";
import { FilePatch } from "./FilePatch";
import { FILE_NAVIGATION, FILE_NAVIGATION_CHANGE } from "./OtherDiscussions";
import { setPatchMatch } from "./patch-decoration";
import { patchDocument } from "./patch-document";
import {
  lineAt,
  matchingFile,
  sourceAt,
  sourceRect,
  sourceRanges,
  storedFile,
  storedPatch,
  storePatch,
} from "./patch-position";
import type { PatchLine, SourcePosition, ThreadJump } from "./patch-position";
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
  readonly toolbar: RefObject<HTMLDivElement | null>;
  readonly navigation: HTMLDivElement | null;
  readonly threadJump: ThreadJump | undefined;
  readonly onThreadJump: (target: ThreadJump | undefined) => void;
  readonly onLoadedAnchors: (threads: ReadonlySet<string> | undefined) => void;
  readonly restore: boolean;
  readonly onReload: () => void;
};

type Jump = {
  readonly index: number;
  readonly inset: number;
  readonly focus: boolean;
  readonly line?: PatchLine | undefined;
  readonly source?: SourcePosition | undefined;
  readonly match?: CodeTarget | undefined;
};
type CodeTarget = { readonly match: PatchSearchMatch; readonly query: string };
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
  toolbar,
  navigation,
  threadJump,
  onThreadJump,
  onLoadedAnchors,
  restore,
  onReload,
}: Props) {
  const [host, setHost] = useState<HTMLDivElement | null>(null);
  const [geometry, setGeometry] = useState({ margin: 0, toolbar: 0 });
  const visibleOrdinals = useRef<readonly number[]>([]);
  const [pinned, setPinned] = useState<readonly number[]>([]);
  const index = usePatchPages(reviewId, initial, pinned, visibleOrdinals);
  const generation = useRef(0);
  const advanceGeneration = useCallback(() => ++generation.current, []);
  const [navigationError, setNavigationError] = useState<string>();
  useEffect(() => () => setPatchMatch([]), []);
  const [landed, setLanded] = useState<{ readonly index: number; readonly offset: number }>();
  const [jump, setJump] = useState<Jump | undefined>(() => {
    if (!restore || threadJump) return;
    const saved = storedPatch(reviewId, initial.revision);
    const file = initial.items.findIndex(
      (entry) => entry.fileId === (saved?.fileId ?? storedFile(reviewId)),
    );
    if (
      file < 0 ||
      (!saved?.inset && !saved?.line && !saved?.source && initial.pageNumber === 1 && file === 0)
    )
      return;
    return {
      index: pageStart(initial.pageNumber) + file,
      inset: saved?.inset ?? 0,
      focus: false,
      ...(saved?.line && { line: saved.line }),
      ...(saved?.source && { source: saved.source }),
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

  const knownExtents = useRef(new Set<number>());
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
    estimateSize: () => estimateFile(undefined),
    overscan: 4,
    rangeExtractor,
    scrollMargin: geometry.margin,
    scrollPaddingStart: geometry.toolbar,
  });
  useLayoutEffect(() => {
    list.shouldAdjustScrollPositionOnItemSizeChange = (item, delta, virtualizer) => {
      const slot = virtualizer.elementsCache.get(item.key);
      if (slot instanceof HTMLElement && slot.dataset.sourceAnchorHeight !== undefined) {
        const height = Number(slot.dataset.sourceAnchorHeight);
        delete slot.dataset.sourceAnchorHeight;
        if (item.size + delta === height) return true;
      }
      return item.end <= (virtualizer.scrollOffset ?? 0) + geometry.toolbar;
    };
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
  useLayoutEffect(() => {
    visibleOrdinals.current = [viewportFirst, last, ...(jump ? [jump.index] : [])];
  }, [viewportFirst, last, jump]);
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
  }, [host, scroller, toolbar]);

  useLayoutEffect(() => {
    for (const [ordinal, file] of files) {
      if (knownExtents.current.has(ordinal)) continue;
      knownExtents.current.add(ordinal);
      if (!list.elementsCache.has(ordinal)) list.resizeItem(ordinal, estimateFile(file));
    }
  }, [files, list]);

  useLayoutEffect(() => {
    if (!jump || !host || !scroller || !files.has(jump.index)) return;
    const scroll = scroller;
    const file = host.querySelector<HTMLElement>(`[data-file-position="${jump.index + 1}"]`);
    if (!file) return;
    const requested = generation.current;
    let frame = 0;
    const observer = new ResizeObserver(schedule);
    const mutations = new MutationObserver(schedule);
    observer.observe(file);
    mutations.observe(file, {
      childList: true,
      subtree: true,
      attributes: true,
      attributeFilter: ["data-patch-prepared", "data-source-seek"],
    });
    schedule();
    return () => {
      cancelAnimationFrame(frame);
      mutations.disconnect();
      observer.disconnect();
    };

    function schedule() {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (requested !== generation.current || !file?.isConnected) return;
        if (file.querySelector(".patch-waiting, .patch-rich:not([data-patch-prepared])")) return;
        // A newly pinned file can paint beyond its estimated slot before ResizeObserver
        // commits the real size. Scrolling there early can push the sticky header out of its parent.
        const slot = file.parentElement;
        const placement = virtual.find((item) => item.index === jump?.index);
        if (slot && placement && Math.abs(slot.offsetHeight - placement.size) > 1) return;
        const rich = file.querySelector<HTMLElement>(".patch-rich");
        if (jump?.source && !rich && file.querySelector("[data-source-seek='pending']")) return;
        const line = jump?.line && rich && patchDocument(rich)?.line(jump.line);
        const row = line?.element;
        const source = jump?.source && sourceRect(file, jump.source);
        const code = jump?.match;
        const match =
          code &&
          rich &&
          patchDocument(rich)?.match({
            line: { side: code.match.side, number: code.match.line },
            hunk: code.match.hunk,
            start: code.match.sourceStart,
            end: code.match.sourceEnd,
          });
        const rawMatches =
          code && jump.source ? sourceRanges(file, jump.source, code.match.sourceEnd) : [];
        const ranges = match ? [match] : rawMatches;
        const confirmedMatch =
          code && ranges.map((range) => range.toString()).join("") === code.query;
        const target = (confirmedMatch && ranges[0]?.getClientRects()[0]) || source || line?.rect;
        if (code && !confirmedMatch)
          setNavigationError("The match is outside the available patch.");
        const viewTop = scroll.getBoundingClientRect().top;
        const readingTop = () =>
          Math.max(viewTop, toolbar.current?.getBoundingClientRect().bottom ?? viewTop);
        const inset = target
          ? -(jump?.focus ? 5 : (jump?.source?.inset ?? jump?.line?.inset ?? 0))
          : Math.min(jump?.inset ?? 0, Math.max(0, file.offsetHeight - 30));
        const before = scroll.scrollTop;
        const destination = (target?.top ?? file.getBoundingClientRect().top) + inset;
        scroll.scrollTop += destination - readingTop();
        // The header can move from its natural position to sticky during landing.
        const residual = destination - (scroll.scrollTop - before) - readingTop();
        if (Math.abs(residual) > 0.5) scroll.scrollTop += residual;
        // A distant seek changes which neighboring files the virtualizer measures.
        // Confirm the glyph against the updated viewport before handing keyboard focus to it.
        if (target && jump?.focus && Math.abs(scroll.scrollTop - before) > 1) {
          schedule();
          return;
        }
        setPatchMatch(confirmedMatch ? ranges : []);
        if (!target && jump?.focus && !code && (jump.line || jump.source))
          setNavigationError("This line is outside the available patch.");
        if (jump?.focus) {
          const focus =
            row ||
            (confirmedMatch && file.querySelector<HTMLElement>("pre[data-patch-etag]")) ||
            file.querySelector<HTMLElement>(".patch-file-title");
          if (focus) {
            focus.tabIndex = -1;
            focus.focus({ preventScroll: true });
          }
        }
        setLanded({ index: jump!.index, offset: scroll.scrollTop });
        setJump(undefined);
        setFileNavigation(scroll, false);
      });
    }
  }, [files, geometry.toolbar, host, jump, scroller, toolbar, virtual]);

  const cancelNavigation = useEffectEvent((event: Event) => {
    if (!event.isTrusted) return;
    if (
      event instanceof KeyboardEvent &&
      (!["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End", " "].includes(event.key) ||
        (event.target instanceof Element &&
          event.target.closest("button, a, input, summary, select, textarea")))
    )
      return;
    if (
      event instanceof PointerEvent &&
      event.target instanceof Element &&
      event.target.closest(".changes-header, .changes-conversation")
    )
      return;
    interruptNavigation();
  });
  useLayoutEffect(() => {
    if (!scroller) return;
    const signals = new AbortController();
    const options = { signal: signals.signal, passive: true };
    for (const event of ["wheel", "pointerdown", "touchstart", "keydown"])
      scroller.addEventListener(event, cancelNavigation, options);
    return () => {
      signals.abort();
      advanceGeneration();
      setFileNavigation(scroller, false);
    };
  }, [advanceGeneration, scroller]);

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
      const viewTop = scroller.getBoundingClientRect().top;
      const top = Math.max(viewTop, toolbar.current?.getBoundingClientRect().bottom ?? viewTop);
      const inset = top - item.getBoundingClientRect().top;
      const line = lineAt(item, scroller, top - viewTop);
      const source = sourceAt(item, scroller, top - viewTop);
      storePatch(reviewId, initial.revision, {
        fileId: active.fileId,
        inset: Math.max(0, inset),
        ...(line && { line }),
        ...(source && { source }),
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

  const pastFiles = offset + geometry.toolbar >= geometry.margin + list.getTotalSize();
  const fetchNearby = useEffectEvent(() => {
    if (index.pending || index.error || !scroller || !count || jump) return;
    if (
      scroller.hasAttribute("data-reading-discussions") ||
      (pastFiles && scroller.querySelector(".discussion-list .changes-conversation"))
    )
      return;
    const start = viewportFirst;
    const finish = last;
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
    pastFiles,
    scroller,
  ]);

  function beginNavigation() {
    setFileNavigation(scroller, true);
    index.cancelManual();
    setNavigationError(undefined);
    setJump(undefined);
    return advanceGeneration();
  }

  function interruptNavigation() {
    advanceGeneration();
    index.cancelManual();
    setJump(undefined);
    setFileNavigation(scroller, false);
  }

  function goTo(ordinal: number, focus: boolean, line?: PatchLine, source?: SourcePosition) {
    setJump({ index: ordinal, inset: 0, focus, line, source });
  }

  function chooseFile(file: PatchFile) {
    beginNavigation();
    for (const [ordinal, entry] of files)
      if (entry.fileId === file.fileId) {
        goTo(ordinal, true);
        return;
      }
    setFileNavigation(scroller, false);
  }

  async function chooseTarget(
    target:
      | { readonly path: PatchPath }
      | { readonly fileId: string }
      | { readonly paths: Pick<PatchPath, "oldPath" | "newPath"> },
    line?: PatchLine,
    match?: CodeTarget,
  ) {
    const command = beginNavigation();
    const matches = (file: PatchFile) => {
      if ("fileId" in target) return file.fileId === target.fileId;
      const path = "path" in target ? target.path : target.paths;
      return file.oldPath === path.oldPath && file.newPath === path.newPath;
    };
    for (const [ordinal, file] of files)
      if (matches(file) && (!match || file.patchETag === match.match.patchETag)) {
        openTarget(ordinal, file);
        return;
      }
    const page = await index.load(target, true);
    if (generation.current !== command) return;
    const offset = page?.items.findIndex(matches) ?? -1;
    if (page && offset >= 0) openTarget(pageStart(page.pageNumber) + offset, page.items[offset]!);
    else setFileNavigation(scroller, false);

    function openTarget(ordinal: number, file: PatchFile) {
      if (!match) {
        goTo(ordinal, true, line);
        return;
      }
      if (file.patchETag !== match.match.patchETag) {
        setNavigationError("The patch changed since this match. Search again.");
        setFileNavigation(scroller, false);
        return;
      }
      const source = file.patchETag
        ? {
            byte: match.match.sourceStart,
            end: match.match.sourceEnd,
            first: Math.max(0, match.match.sourceStart - 8192),
            etag: file.patchETag,
            inset: 0,
          }
        : undefined;
      setJump({ index: ordinal, inset: 0, focus: true, line, source, match });
    }
  }

  const chooseThread = useEffectEvent((target: ThreadJump) => {
    onThreadJump(undefined);
    void chooseTarget(target, target.line);
  });
  useLayoutEffect(() => {
    if (!threadJump) return;
    const command = generation.current;
    const frame = requestAnimationFrame(() => {
      if (command === generation.current) chooseThread(threadJump);
      else onThreadJump(undefined);
    });
    return () => cancelAnimationFrame(frame);
  }, [onThreadJump, threadJump]);

  async function goToOrdinal(ordinal: number) {
    if (ordinal < 0 || ordinal >= count) return;
    const command = beginNavigation();
    if (files.has(ordinal)) {
      goTo(ordinal, false);
      return;
    }
    const target = index.lookupPage(pageAt(ordinal));
    if (target && target.page === pageAt(ordinal)) {
      const page = await index.load(target, true);
      if (generation.current !== command) return;
      if (page?.items[ordinal - pageStart(page.pageNumber)]) {
        goTo(ordinal, false);
        return;
      }
    }
    setFileNavigation(scroller, false);
  }

  return (
    <div className="file-stream">
      {navigation &&
        count > 0 &&
        createPortal(
          <ChangedFilePicker
            reviewId={reviewId}
            index={shown}
            files={files}
            selected={active}
            position={first + 1}
            total={total}
            pending={index.pending && !active}
            catalog={catalog}
            navigation={{
              previous: first > 0,
              next: first + 1 < count,
              onStep: (direction) => void goToOrdinal(first + direction),
            }}
            onOpen={() => {
              interruptNavigation();
              setNavigationError(undefined);
            }}
            onChooseFile={chooseFile}
            onChoosePath={(path) => void chooseTarget({ path })}
            onChooseMatch={(match, query) =>
              void chooseTarget(
                { fileId: match.fileId },
                { side: match.side, number: match.line, inset: 0 },
                { match, query },
              )
            }
          />,
          navigation,
        )}
      {navigationError && <Notice>{navigationError}</Notice>}
      {jump && (
        <p className="sr-only" role="status">
          Opening the selected patch…
        </p>
      )}
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
                  key={JSON.stringify([file.revision, file.fileId, file.patchETag])}
                  file={file}
                  threads={anchors.get(item.index) ?? EMPTY_THREADS}
                  source={source}
                  position={item.index + 1}
                  admitted={wanted.has(item.index)}
                  entrySide={
                    item.index < (jump?.index ?? viewportFirst)
                      ? "end"
                      : item.index > (jump?.index ?? last)
                        ? "start"
                        : undefined
                  }
                  sourcePosition={
                    jump
                      ? item.index === jump.index
                        ? (jump.source ?? "start")
                        : "away"
                      : undefined
                  }
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

function setFileNavigation(scroller: Element | null, pending: boolean) {
  if (!scroller || scroller.hasAttribute(FILE_NAVIGATION) === pending) return;
  scroller.toggleAttribute(FILE_NAVIGATION, pending);
  scroller.dispatchEvent(new Event(FILE_NAVIGATION_CHANGE));
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
    const size = isRichPatch(file)
      ? (file.byteLength ?? MAX_ACTIVE_BYTES)
      : Math.min(file.byteLength ?? Infinity, PATCH_WINDOW_BYTES * 4);
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
