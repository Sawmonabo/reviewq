import { useVirtualizer } from "@tanstack/react-virtual";
import type { defaultRangeExtractor } from "@tanstack/react-virtual";
import {
  useCallback,
  useDeferredValue,
  useEffect,
  useEffectEvent,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { HistoryPart } from "../../../api/generated/contracts";
import type { HostProviderDto } from "../../../api/generated/contracts/common";
import type { MessageHistoryItem } from "../../../api/generated/contracts/delivery";
import { historyParts } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { describeError, readable } from "../../../lib/presentation";
import { deliveryLabels } from "../delivery-labels";
import { EntryBody } from "./EntryBody";
import { HistorySelection } from "./HistorySelection";
import { HistoryStatus } from "./HistoryStatus";
import { TranscriptFind } from "./find/TranscriptFind";
import type { FoundOutput, PreparedDestination } from "./find/destination";
import {
  entriesIn,
  historyBookmark,
  readBookmark,
  rememberBookmark,
  sameBookmark,
  visibleAnchor,
  type ReadingAnchor,
  type TimelineBookmark,
} from "./history-window";
import { ProviderMark } from "./ProviderMark";
import { PromptNavigator } from "./PromptNavigator";
import { HistoryTimestamp } from "./HistoryTimestamp";
import { FileReadPack, ToolRunHeader } from "./ToolActivity";
import { openKey, toolCatalog, toolOnly } from "./tool-activity";
import type { ReadTarget, ToolRun } from "./tool-activity";
import {
  historyId,
  openToolRun,
  promptRanks,
  transcriptRangeIndexes,
  readingIndex,
  timelineRows,
  toggleKey,
  type VisibleRow,
} from "./timeline-rows";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import { useHistory } from "./useHistory";
import { useInlineProse } from "./useInlineProse";

type Position = ReadingAnchor & {
  readonly reopenPages?: number;
  readonly jump?: boolean;
  readonly adjacent?: "read" | "adjusting";
};
const SCROLL_IDLE_MS = 160;
const MAX_REOPEN_PAGES = 4;
type ExtraParts = {
  readonly entryId: string;
  readonly generation: string;
  readonly pages: number;
  readonly items: readonly HistoryPart[];
  readonly next: string | null;
};
type OutputSelection = { readonly entryId: string; readonly reference: string };
type Props = {
  readonly target: SessionAddress;
  readonly provider: HostProviderDto;
  readonly child: boolean;
  readonly messages: readonly MessageHistoryItem[];
  readonly working: boolean;
};

export function Transcript({ target, provider, child, messages, working }: Props) {
  const providerName = readable(provider);
  const bookmark = `history.${target.runId}.${target.sessionKey}`;
  const [saved] = useState(() => readBookmark(bookmark));
  const [following, setFollowing] = useState(saved?.following ?? true);
  const followBeforeFind = useRef<boolean | undefined>(undefined);
  const [pinned, setPinned] = useState<ReadonlySet<string>>(() => new Set());
  const [selectedText, setSelectedText] = useState(false);
  const [diagramFocus, setDiagramFocus] = useState<string>();
  const focusedDiagram = !selectedText && pinned.size === 1 ? diagramFocus : undefined;
  const [groupKeys, setGroupKeys] = useState<ReadonlySet<string>>(() => new Set());
  const [actionNotice, setActionNotice] = useState<string>();
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(
    () =>
      new Set(
        saved && !saved.following && saved.callId ? [openKey(saved.entryId, saved.callId)] : [],
      ),
  );
  const [expandedRuns, setExpandedRuns] = useState<ReadonlyMap<string, boolean>>(
    () =>
      new Map(
        saved && !saved.following && saved.toolEntry !== false ? [[saved.entryId, true]] : [],
      ),
  );
  const [expandedReads, setExpandedReads] = useState<ReadonlySet<string>>(
    () => new Set(saved && !saved.following && saved.toolEntry !== false ? [saved.entryId] : []),
  );
  const [output, setOutput] = useState<OutputSelection>();
  const [foundOutput, setFoundOutput] = useState<FoundOutput>();
  const [findAnchorEntry, setFindAnchorEntry] = useState<string>();
  const releaseFindAnchor = useCallback(() => setFindAnchorEntry(undefined), []);
  const [extraParts, setExtraParts] = useState<ExtraParts>();
  const partsRequest = useRef<AbortController | undefined>(undefined);
  const viewport = useRef<HTMLDivElement>(null);
  const anchor = useRef<Position | undefined>(
    saved && !saved.following
      ? {
          id: saved.entryId,
          offset: saved.offset,
          toolEntry: saved.toolEntry ?? true,
          reopenPages: 0,
        }
      : undefined,
  );
  const selectionState = useRef<{ hasText: boolean; ids: Set<string>; diagram?: string }>({
    hasText: false,
    ids: new Set(),
  });
  const scrollIntent = useRef<"head" | "tail" | "scrollbar" | undefined>(undefined);
  const pageLead = useRef(160);
  const touchY = useRef<number | undefined>(undefined);
  const readingPosition = useRef<TimelineBookmark | undefined>(undefined);
  const written = useRef(saved);
  const requestedLatest = useRef(false);
  const activeLatest = useRef(false);
  const toolFocus = useRef<ReadTarget | undefined>(undefined);

  const history = useHistory(
    target,
    bookmark,
    following,
    pinned,
    focusedDiagram,
    (position, wanted, extend) => {
      // Keep mounted group controls across a prepend or append that extends their native span.
      setGroupKeys(
        new Set(
          extend
            ? entries
                .filter((row) => row.kind === "tool-run" || row.kind === "read-pack")
                .map((row) => row.id)
            : [],
        ),
      );
      if (position === "around" && wanted) {
        if (wanted.locator || anchor.current?.id !== wanted.entryId)
          anchor.current = {
            id: wanted.entryId,
            offset: wanted.locator ? 24 : 0,
            reopenPages: 0,
            jump: true,
          };
      } else if (focusedDiagram && (position === "latest" || position === "newer")) {
        const host = viewport.current;
        const row = host?.querySelector(`[data-native-entry="${CSS.escape(focusedDiagram)}"]`);
        if (host && row)
          anchor.current = {
            id: focusedDiagram,
            offset: row.getBoundingClientRect().top - host.getBoundingClientRect().top,
            toolEntry: true,
            adjacent: "read",
          };
      } else if (position !== "latest" && !following && anchor.current?.reopenPages === undefined) {
        const reading = visibleAnchor(viewport.current);
        anchor.current = reading && (extend ? { ...reading, adjacent: "read" } : reading);
      }
    },
  );
  const historyEntries = useMemo(() => entriesIn(history.pages), [history.pages]);
  const knownTools = useMemo(() => toolCatalog(historyEntries), [historyEntries]);
  useEffect(() => () => partsRequest.current?.abort(), [history.generation]);
  const canonical = useMemo(
    () => new Set(historyEntries.map((item) => item.entryId)),
    [historyEntries],
  );
  const streamed = useDeferredValue(history.live);
  const inProgress =
    streamed && streamed.entryId === history.live?.entryId && !canonical.has(streamed.entryId)
      ? streamed
      : undefined;
  const entries: VisibleRow[] = useMemo(() => {
    const rows = timelineRows(
      historyEntries,
      knownTools,
      expandedRuns,
      expandedReads,
      expanded,
      history.generation ? messages : [],
      history.older,
      history.newer,
      Boolean(inProgress),
      groupKeys,
    );
    if (!history.newer && inProgress)
      rows.push({ kind: "live", id: inProgress.entryId, provisional: inProgress });
    return rows;
  }, [
    expanded,
    expandedReads,
    expandedRuns,
    groupKeys,
    history.generation,
    history.newer,
    history.older,
    historyEntries,
    inProgress,
    knownTools,
    messages,
  ]);

  const rangeExtractor = useCallback(
    (range: Parameters<typeof defaultRangeExtractor>[0]) =>
      transcriptRangeIndexes(range, entries, pinned, anchor.current, findAnchorEntry),
    [entries, pinned, findAnchorEntry],
  );

  const rowKey = useCallback((index: number) => entries[index]?.id ?? index, [entries]);

  // Keep scroll measurements and the TanStack instance within this component.
  // eslint-disable-next-line react-hooks/incompatible-library
  const list = useVirtualizer({
    count: entries.length,
    getScrollElement: () => viewport.current,
    getItemKey: rowKey,
    // Resolve retained row keys against the new window before mounting its visible range.
    anchorTo: focusedDiagram ? "start" : "end",
    estimateSize: (index) =>
      entries[index]?.kind === "tool-run" ? 35 : entries[index]?.kind === "read-pack" ? 55 : 95,
    paddingStart: history.older ? 40 : 0,
    paddingEnd: history.newer ? 40 : 0,
    overscan: 7,
    rangeExtractor,
  });

  useEffect(() => {
    // Heights belong to the retained pages. Keep measurements for pinned and overlapping rows.
    const retained = new Set(entries.map((row) => row.id));
    for (const key of list.itemSizeCache.keys()) {
      if (typeof key !== "string" || !retained.has(key)) list.itemSizeCache.delete(key);
    }
  }, [entries, list]);

  const prompts = useMemo(() => promptRanks(entries), [entries]);
  const virtualRows = list.getVirtualItems();
  const windowHeight = list.scrollRect?.height ?? 0;
  const windowTop = following
    ? Math.max(0, list.getTotalSize() - windowHeight)
    : (list.scrollOffset ?? 0);
  const nearby = windowHeight
    ? virtualRows
        .filter((item) => item.end > windowTop - 120 && item.start < windowTop + windowHeight + 120)
        .flatMap((item) => {
          const row = entries[item.index];
          return row?.kind === "native" && row.entry ? [row.entry] : [];
        })
    : [];
  const prose = useInlineProse(
    target,
    history.generation,
    following ? nearby.reverse() : nearby,
    selectedText || (pinned.size > 0 && !focusedDiagram),
    output?.reference,
  );

  const extendReopen = useEffectEvent((position: Position, direction: "older" | "newer") => {
    if (
      position.reopenPages === undefined ||
      position.reopenPages >= MAX_REOPEN_PAGES ||
      history.loading ||
      !history[direction]
    )
      return;
    anchor.current = { ...position, reopenPages: position.reopenPages + 1 };
    void history.load(direction);
  });

  const contentHeight = list.getTotalSize();
  useLayoutEffect(() => {
    if (!entries.length) return;
    if (requestedLatest.current || (following && !anchor.current && !focusedDiagram)) {
      list.scrollToIndex(entries.length - 1, { align: "end" });
      requestedLatest.current = false;
      anchor.current = undefined;
    } else if (anchor.current) {
      const saved = anchor.current;
      const focus = toolFocus.current?.entryId === saved.id ? toolFocus.current : undefined;
      const exactEntry = Boolean(focus || saved.toolEntry);
      const index = readingIndex(entries, saved, exactEntry);
      const targetRow = entries[index];
      const container = viewport.current;
      if (saved.adjacent) {
        // A page can remove the head button or insert children between a group header
        // and a focused tool. Correct only the inset the keyed virtualizer didn't preserve.
        const row =
          targetRow && container?.querySelector(`[data-entry="${CSS.escape(targetRow.id)}"]`);
        if (!container || !row) {
          anchor.current = undefined;
          return;
        }
        const delta =
          row.getBoundingClientRect().top - container.getBoundingClientRect().top - saved.offset;
        if (Math.abs(delta) > 1) {
          anchor.current = { ...saved, adjacent: "adjusting" };
          list.scrollToOffset(container.scrollTop + delta);
        } else if (saved.adjacent === "read") {
          anchor.current = undefined;
          return;
        }
        // Inserted rows can settle at different heights after their first measurement.
        // Only this displaced reading position stays active until layout settles or new input.
        const held = anchor.current;
        const settled = window.setTimeout(() => {
          if (anchor.current === held) anchor.current = undefined;
        }, SCROLL_IDLE_MS);
        return () => window.clearTimeout(settled);
      }
      if (!targetRow) return;
      const targetId = exactEntry ? saved.id : historyId(targetRow);
      const selector = `[${exactEntry ? "data-entry" : "data-native-entry"}="${CSS.escape(targetId)}"]`;
      if (!container?.querySelector(selector)) list.scrollToIndex(index, { align: "start" });
      const frame = requestAnimationFrame(() => {
        if (!container?.isConnected || anchor.current !== saved) return;
        const row = container.querySelector<HTMLElement>(selector);
        if (!row) return;
        list.cancelScroll();
        const wanted =
          container.scrollTop +
          row.getBoundingClientRect().top -
          container.getBoundingClientRect().top -
          saved.offset;
        const max = container.scrollHeight - container.clientHeight;
        if (Math.abs(wanted - container.scrollTop) > 1) container.scrollTop = wanted;
        // The surrounding page or late inline prose can change the
        // scroll bounds. Keep the target until it fits or the reader moves.
        if (saved.reopenPages !== undefined && (wanted > max + 1 || wanted < -1)) {
          extendReopen(saved, wanted < 0 ? "older" : "newer");
          return;
        }
        if (!saved.jump) anchor.current = undefined;
        if (focus && toolFocus.current === focus) {
          const tool = [...(row?.querySelectorAll<HTMLElement>("[data-tool-call]") ?? [])].find(
            (item) => item.dataset.toolCall === focus.callId,
          );
          (tool ?? container).focus({ preventScroll: true });
          toolFocus.current = undefined;
        }
      });
      return () => cancelAnimationFrame(frame);
    }
  }, [entries, following, list, prose, contentHeight, focusedDiagram]);

  useEffect(() => {
    const host = viewport.current;
    if (!host) return;
    function updatePins() {
      if (!host) return;
      const selected = getSelection();
      const ids = new Set<string>();
      let hasText = false;
      if (selected && !selected.isCollapsed && selected.rangeCount) {
        const range = selected.getRangeAt(0);
        hasText = range.intersectsNode(host);
        if (hasText)
          for (const node of host.querySelectorAll<HTMLElement>("[data-native-entry]")) {
            if (node.dataset.nativeEntry && range.intersectsNode(node))
              ids.add(node.dataset.nativeEntry);
          }
      }
      const active = document.activeElement;
      const focused = active?.closest<HTMLElement>("[data-native-entry]");
      const id = focused && host.contains(focused) ? focused.dataset.nativeEntry : undefined;
      if (id) ids.add(id);
      const previous = selectionState.current;
      const diagram =
        id &&
        (previous.diagram === id || focused?.hasAttribute("data-live-entry")) &&
        active?.closest(".mermaid-figure")
          ? id
          : undefined;
      if (
        previous.hasText !== hasText ||
        previous.diagram !== diagram ||
        previous.ids.size !== ids.size ||
        [...ids].some((id) => !previous.ids.has(id))
      ) {
        selectionState.current = { hasText, ids, ...(diagram ? { diagram } : {}) };
        setPinned(ids);
        setSelectedText(hasText);
        setDiagramFocus(diagram);
      }
    }
    document.addEventListener("selectionchange", updatePins);
    host.addEventListener("focusin", updatePins);
    host.addEventListener("focusout", updatePins);
    return () => {
      document.removeEventListener("selectionchange", updatePins);
      host.removeEventListener("focusin", updatePins);
      host.removeEventListener("focusout", updatePins);
    };
  }, []);

  const savePosition = useEffectEvent((host: HTMLDivElement | null) => {
    if (anchor.current?.reopenPages !== undefined && !anchor.current.jump) return;
    const current = historyBookmark(host, history.generation, following);
    const previous = readingPosition.current;
    const position =
      current ?? (previous?.generation === history.generation ? previous : undefined);
    if (position && !sameBookmark(position, written.current)) {
      rememberBookmark(bookmark, position);
      written.current = position;
    }
  });

  useLayoutEffect(() => {
    const host = viewport.current;
    let timer: number | undefined;
    function flush() {
      window.clearTimeout(timer);
      timer = undefined;
      scrollIntent.current = undefined;
      savePosition(host);
    }
    function settle() {
      window.clearTimeout(timer);
      timer = window.setTimeout(flush, SCROLL_IDLE_MS);
    }
    const showThumb = () => host?.classList.add("is-scrolling");
    const hideThumb = () => {
      host?.classList.remove("is-scrolling");
      settle();
    };
    host?.addEventListener("scroll", showThumb, { passive: true });
    host?.addEventListener("scrollend", hideThumb);
    host?.addEventListener("wheel", settle, { passive: true });
    host?.addEventListener("touchmove", settle, { passive: true });
    window.addEventListener("pagehide", flush);
    return () => {
      flush();
      host?.removeEventListener("scroll", showThumb);
      host?.removeEventListener("scrollend", hideThumb);
      host?.removeEventListener("wheel", settle);
      host?.removeEventListener("touchmove", settle);
      window.removeEventListener("pagehide", flush);
    };
  }, [bookmark]);

  function leaveReopen() {
    if (
      anchor.current?.jump ||
      anchor.current?.reopenPages !== undefined ||
      anchor.current?.adjacent
    )
      anchor.current = undefined;
  }

  function approachEdge(host: HTMLDivElement, direction: "head" | "tail" | "scrollbar") {
    if (history.loading) return;
    const lead = pinned.size ? 160 : pageLead.current;
    if (direction !== "tail" && history.older && host.scrollTop < lead) {
      void history.load("older");
    } else if (
      direction !== "head" &&
      history.newer &&
      host.scrollHeight - host.clientHeight - host.scrollTop < lead
    ) {
      void history.load("newer");
    }
  }

  function recordScroll() {
    const host = viewport.current;
    if (!host) return;
    const nearEnd = host.scrollHeight - host.clientHeight - host.scrollTop < 42;
    const intent = scrollIntent.current;
    let nextFollowing = following;
    if (intent) {
      nextFollowing = intent !== "head" && nearEnd && !history.newer;
      approachEdge(host, intent);
      setFollowing(nextFollowing);
    }
    const position = historyBookmark(host, history.generation, nextFollowing);
    if (position) readingPosition.current = position;
  }

  function gesture(up: boolean, distance?: number) {
    followBeforeFind.current = undefined;
    leaveReopen();
    const host = viewport.current;
    if (!host) return;
    scrollIntent.current = up ? "head" : "tail";
    pageLead.current = Math.max(
      160,
      Math.min(host.clientHeight * 3, (distance ?? host.clientHeight) * 3),
    );
    history.cancelOpposite(up);
    if (up && (host.scrollTop > 0 || history.older) && (following || requestedLatest.current)) {
      requestedLatest.current = false;
      list.cancelScroll();
      setFollowing(false);
    }
    approachEdge(host, up ? "head" : "tail");
  }

  function clear() {
    if (selectedText) getSelection()?.removeAllRanges();
    setSelectedText(false);
    setDiagramFocus(undefined);
    setPinned(new Set());
    selectionState.current = { hasText: false, ids: new Set() };
  }

  function latest() {
    followBeforeFind.current = undefined;
    leaveReopen();
    clear();
    setFollowing(true);
    requestedLatest.current = true;
    activeLatest.current = true;
    if (!selectedText && !pinned.size) {
      activeLatest.current = false;
      void history.load("latest");
    }
  }

  useEffect(() => {
    if (activeLatest.current && !pinned.size && !history.loading) {
      activeLatest.current = false;
      void history.load("latest");
    }
  }, [pinned, history]);

  async function moreParts(entryId: string, cursor: string) {
    const generation = history.generation;
    if (!generation || partsRequest.current) return;
    if (extraParts && extraParts.entryId !== entryId && pinned.has(extraParts.entryId)) {
      setActionNotice("Clear the active selection before opening another long entry.");
      return;
    }
    const prior =
      extraParts?.generation === generation && extraParts.entryId === entryId
        ? extraParts
        : undefined;
    if (prior && prior.pages >= 4) {
      setActionNotice(
        "This entry has more parts than fit in one window. Use its complete output or open the session in the terminal.",
      );
      return;
    }
    const controller = new AbortController();
    partsRequest.current = controller;
    setActionNotice(undefined);
    try {
      const result = await historyParts(target, entryId, cursor, controller.signal);
      if (!controller.signal.aborted)
        setExtraParts({
          entryId,
          generation,
          pages: (prior?.pages ?? 0) + 1,
          items: [...(prior?.items ?? []), ...result.parts],
          next: result.nextCursor,
        });
    } catch (error) {
      if (!controller.signal.aborted) setActionNotice(describeError(error));
    } finally {
      if (partsRequest.current === controller) partsRequest.current = undefined;
    }
  }

  function expandRun(run: ToolRun) {
    const host = viewport.current;
    const header = host?.querySelector<HTMLElement>(`[data-entry="${CSS.escape(run.id)}"]`);
    if (host && header)
      anchor.current = {
        id: run.id,
        toolEntry: true,
        offset: header.getBoundingClientRect().top - host.getBoundingClientRect().top,
      };
    setExpandedRuns((known) => {
      const next = new Map(known);
      for (const id of run.members) next.delete(id);
      next.set(run.anchorId, !run.open);
      return next.size > 160 ? new Map([...next].slice(-160)) : next;
    });
  }

  function inspectRead(at: ReadTarget) {
    const host = viewport.current;
    const active = document.activeElement?.closest<HTMLElement>("[data-native-entry]");
    anchor.current = {
      id: at.entryId,
      toolEntry: true,
      offset:
        active && host ? active.getBoundingClientRect().top - host.getBoundingClientRect().top : 0,
    };
    toolFocus.current = at;
    setExpandedReads((known) => toggleKey(known, at.entryId, true));
    setExpanded((known) => toggleKey(known, openKey(at.entryId, at.callId), true));
  }

  function holdOutputRange(element: HTMLElement) {
    const host = viewport.current;
    const row = element.closest<HTMLElement>("[data-entry]");
    const id = row?.dataset.entry;
    if ((!following && !requestedLatest.current) || !host || !row || !id || !host.contains(row))
      return false;
    anchor.current = {
      id,
      toolEntry: true,
      offset: row.getBoundingClientRect().top - host.getBoundingClientRect().top,
    };
    scrollIntent.current = undefined;
    requestedLatest.current = false;
    list.cancelScroll();
    setFollowing(false);
    return true;
  }

  function openFound({ page, output: found, extra }: PreparedDestination) {
    const { entryId, generation, locator, contentRef, toolCallId } = found.destination;
    if (!history.openPrepared(page, { entryId, locator })) return false;
    followBeforeFind.current = undefined;
    setFollowing(false);
    setFoundOutput(found);
    setFindAnchorEntry(entryId);
    setOutput({ entryId, reference: contentRef });
    if (extra) setExtraParts({ entryId, generation, pages: 1, ...extra });
    if (toolCallId) {
      setExpanded((known) => toggleKey(known, openKey(entryId, toolCallId), true));
      setExpandedReads((known) => toggleKey(known, entryId, true));
      setExpandedRuns((known) => openToolRun(entries, known, entryId));
    }
    return true;
  }

  return (
    <section className="transcript-shell" aria-label="Native conversation">
      <TranscriptFind
        target={target}
        viewport={viewport}
        onOpen={() => {
          followBeforeFind.current = following;
          const reading = visibleAnchor(viewport.current);
          anchor.current = reading && { ...reading, jump: true };
          list.cancelScroll();
          requestedLatest.current = false;
          setFollowing(false);
        }}
        onClose={() => {
          leaveReopen();
          if (followBeforeFind.current) setFollowing(true);
          followBeforeFind.current = undefined;
        }}
        onChoose={openFound}
        onReveal={leaveReopen}
        onRelease={releaseFindAnchor}
      />
      {selectedText && <HistorySelection onClear={clear} onError={setActionNotice} />}
      {actionNotice && <Notice>{actionNotice}</Notice>}
      <HistoryStatus history={history} onRetry={() => void history.load("latest")} />
      <div
        className="timeline-viewport"
        aria-label="Conversation history"
        role="region"
        ref={viewport}
        tabIndex={0}
        onScroll={recordScroll}
        onWheel={(event) => {
          if (event.deltaY && !event.ctrlKey) {
            const unit =
              event.deltaMode === 2
                ? event.currentTarget.clientHeight
                : event.deltaMode === 1
                  ? 16
                  : 1;
            gesture(event.deltaY < 0, Math.abs(event.deltaY) * unit);
          }
        }}
        onTouchStart={(event) => {
          touchY.current = event.touches.length === 1 ? event.touches[0]?.clientY : undefined;
        }}
        onTouchMove={(event) => {
          const previous = touchY.current;
          const current = event.touches.length === 1 ? event.touches[0]?.clientY : undefined;
          touchY.current = current;
          if (previous !== undefined && current !== undefined && current !== previous)
            gesture(current > previous, Math.abs(current - previous));
        }}
        onPointerDown={(event) => {
          if (anchor.current?.adjacent) anchor.current = undefined;
          const box = event.currentTarget.getBoundingClientRect();
          if (event.target === event.currentTarget && box.right - event.clientX < 17) {
            followBeforeFind.current = undefined;
            leaveReopen();
            pageLead.current = event.currentTarget.clientHeight;
            scrollIntent.current = "scrollbar";
          }
        }}
        onKeyDown={(event) => {
          if (anchor.current?.adjacent) anchor.current = undefined;
          if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
          if (event.key === "End" && !event.shiftKey && event.target === event.currentTarget) {
            event.preventDefault();
            scrollIntent.current = undefined;
            latest();
            return;
          }
          if (event.shiftKey && event.key !== " ") return;
          if (event.key === " " && event.target !== event.currentTarget) return;
          if (
            ["PageUp", "ArrowUp", "Home"].includes(event.key) ||
            (event.key === " " && event.shiftKey)
          )
            gesture(true, event.key === "ArrowUp" ? 40 : undefined);
          else if (["PageDown", "ArrowDown", "End", " "].includes(event.key))
            gesture(false, event.key === "ArrowDown" ? 40 : undefined);
        }}
      >
        {!entries.length &&
          (history.loading ? (
            <Loading label="Reading native conversation…" />
          ) : (
            !history.error && <Empty title="No native messages are available" />
          ))}
        {entries.length > 0 && (
          <div className="timeline-spacer" style={{ height: list.getTotalSize() }}>
            {history.older && (
              <button
                className="timeline-boundary older"
                type="button"
                disabled={history.loading}
                onClick={() => {
                  setFollowing(false);
                  void history.load("older");
                }}
              >
                Show earlier entries
              </button>
            )}
            {virtualRows.map((item) => {
              const row = entries[item.index];
              if (!row) return null;
              if (row.kind === "tool-run" || row.kind === "read-pack") {
                return (
                  <article
                    ref={list.measureElement}
                    data-index={item.index}
                    data-entry={row.id}
                    data-native-entry={row.anchorId}
                    data-tool-member={row.kind === "read-pack" ? "" : undefined}
                    key={row.id}
                    className="timeline-entry tool-entry"
                    style={{ transform: `translateY(${item.start}px)` }}
                  >
                    {row.kind === "tool-run" ? (
                      <ToolRunHeader run={row} onToggle={() => expandRun(row)} />
                    ) : (
                      <FileReadPack pack={row} onInspect={inspectRead} />
                    )}
                  </article>
                );
              }
              const justTools = row.entry && toolOnly(row.entry);
              const previous = entries[item.index - 1];
              const followsTool =
                previous &&
                ("anchorId" in previous || (previous.entry && toolOnly(previous.entry)));
              const assignment = Boolean(
                child &&
                row.entry?.parts.length &&
                row.entry.parts.every((part) => part.kind === "assignment"),
              );
              const user = row.entry
                ? row.entry.parts.some((part) => part.kind === "user_prompt")
                : Boolean(row.delivery);
              const followup = user && row.delivery?.purpose === "thread_followup";
              const automatic = followup || (user && row.delivery?.purpose === "codex_recovery");
              const role = assignment
                ? "Assignment"
                : user
                  ? automatic
                    ? followup
                      ? "ReviewQ follow-up"
                      : "ReviewQ"
                    : child
                      ? "Incoming message"
                      : "You"
                  : providerName;
              const timestamp = row.entry?.timestamp ?? row.delivery?.createdAt;
              const found =
                foundOutput?.destination.entryId === row.id &&
                foundOutput.destination.generation === history.generation
                  ? foundOutput
                  : undefined;
              return (
                <article
                  ref={list.measureElement}
                  data-index={item.index}
                  data-entry={row.id}
                  data-native-entry={row.kind === "delivery" ? undefined : row.id}
                  data-live-entry={row.kind === "live" ? "" : undefined}
                  data-tool-member={row.grouped ? "" : undefined}
                  data-after-tools={followsTool && !justTools ? "" : undefined}
                  key={row.id}
                  className={`timeline-entry ${justTools ? "tool-entry" : assignment || user ? "user-entry" : "agent-entry"}`}
                  style={{ transform: `translateY(${item.start}px)` }}
                >
                  {!justTools && (
                    <header className="timeline-message-heading">
                      <span className="timeline-symbol" aria-hidden="true">
                        {assignment ? (
                          "A"
                        ) : user ? (
                          automatic ? (
                            "RQ"
                          ) : (
                            "Y"
                          )
                        ) : (
                          <ProviderMark provider={provider} />
                        )}
                      </span>
                      <strong>{role}</strong>
                      {timestamp && (
                        <HistoryTimestamp timestamp={timestamp} delivery={!row.entry?.timestamp} />
                      )}
                      {row.delivery && (
                        <span className="delivery-status" data-state={row.delivery.state}>
                          {deliveryLabels[row.delivery.state]}
                        </span>
                      )}
                      {row.provisional !== undefined && (
                        <span className="badge green">Working</span>
                      )}
                    </header>
                  )}
                  <div className="message-body stack">
                    {row.delivery && (
                      <div className="timeline-delivery">
                        {row.delivery.attachment?.quote && (
                          <blockquote>
                            <MarkdownContent source={row.delivery.attachment.quote} />
                          </blockquote>
                        )}
                        {!found && <MarkdownContent source={row.delivery.text} />}
                        {row.delivery.outcomeDetail && (
                          <small className="meta">{row.delivery.outcomeDetail}</small>
                        )}
                      </div>
                    )}
                    {(row.entry || row.provisional) &&
                      (!row.delivery || found) &&
                      history.generation && (
                        <EntryBody
                          key={history.generation}
                          target={target}
                          generation={history.generation}
                          entry={row.entry}
                          provisional={row.provisional}
                          knownTools={knownTools}
                          expanded={expanded}
                          prose={prose}
                          toggle={(key, open) =>
                            setExpanded((known) => toggleKey(known, key, open))
                          }
                          reference={output?.entryId === row.id ? output.reference : undefined}
                          {...(extraParts?.generation === history.generation &&
                          extraParts.entryId === row.id
                            ? { extra: extraParts }
                            : {})}
                          onMoreParts={(cursor) => void moreParts(row.id, cursor)}
                          onReadRange={holdOutputRange}
                          {...(found ? { found } : {})}
                          onReference={(reference) => {
                            setFoundOutput(undefined);
                            setOutput(reference ? { entryId: row.id, reference } : undefined);
                          }}
                        />
                      )}
                  </div>
                </article>
              );
            })}
            {history.newer && (
              <button
                className="timeline-boundary newer"
                type="button"
                disabled={history.loading}
                onClick={() => void history.load("newer")}
              >
                Show newer entries
              </button>
            )}
          </div>
        )}
        {working && history.generation && !history.newer && (
          <p className="timeline-working" role="status">
            <span className="timeline-working-dots" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            {providerName} is working
          </p>
        )}
      </div>
      {history.generation && (
        <PromptNavigator
          key={history.generation}
          target={target}
          generation={history.generation}
          following={following}
          rank={prompts.ranks[list.getVirtualItemForOffset(windowTop)?.index ?? 0] ?? 0}
          promptOffsets={prompts.offsets}
          windowCount={prompts.count}
          hasOlder={history.older}
          hasNewer={history.newer}
          onReturnFocus={() => viewport.current?.focus({ preventScroll: true })}
          onJump={(id, locator) => {
            followBeforeFind.current = undefined;
            leaveReopen();
            setFollowing(false);
            void history.load("around", { entryId: id, locator });
          }}
        />
      )}
      {(!following || history.waiting) && (
        <button type="button" className="timeline-jump" onClick={latest}>
          <span aria-hidden="true">↓ </span>
          {history.waiting ? "New messages available" : "Jump to latest"}
        </button>
      )}
    </section>
  );
}
