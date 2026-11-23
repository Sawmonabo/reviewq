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
import { EntryBody } from "./EntryBody";
import { HistorySelection, useHistorySelection } from "./HistorySelection";
import { HistoryEntryHeading, messageAuthor } from "./HistoryEntryHeading";
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
import { PromptNavigator } from "./PromptNavigator";
import { TranscriptJumpControls } from "./TranscriptJumpControls";
import { FileReadPack, ToolRunHeader } from "./ToolActivity";
import { openKey, toolCatalog, toolOnly } from "./tool-activity";
import type { ReadTarget, ToolRun } from "./tool-activity";
import {
  historyId,
  isPinnedRow,
  openToolRun,
  promptRanks,
  transcriptRangeIndexes,
  readingIndex,
  timelineRows,
  toggleKey,
  type VisibleRow,
} from "./timeline-rows";
import { useHistory } from "./useHistory";
import { useInlineProse } from "./useInlineProse";
import { useScrollGestures } from "./useScrollGestures";
import { useTimelineList } from "./useTimelineList";
import { useQuestionJump } from "./questions/useQuestionJump";
import type { Questions } from "./questions/useQuestions";

type Position = ReadingAnchor & {
  readonly reopenPages?: number;
  readonly jump?: boolean;
  readonly adjacent?: "read" | "adjusting";
};
const SCROLL_IDLE_MS = 160;
const MAX_REOPEN_PAGES = 4;
type ExtraParts = {
  readonly rowId: string;
  readonly generation: string;
  readonly pages: number;
  readonly items: readonly HistoryPart[];
  readonly next: string | null;
};
type OutputSelection = { readonly rowId: string; readonly reference: string };
type Props = {
  readonly target: SessionAddress;
  readonly provider: HostProviderDto;
  readonly child: boolean;
  readonly messages: readonly MessageHistoryItem[];
  readonly working: boolean;
  readonly questions: Questions;
};

export function Transcript({ target, provider, child, messages, working, questions }: Props) {
  const providerName = readable(provider);
  const bookmark = `history.${target.runId}.${target.sessionKey}`;
  const [saved] = useState(() => readBookmark(bookmark));
  const [following, setFollowing] = useState(saved?.following ?? true);
  const followBeforeFind = useRef<boolean | undefined>(undefined);
  const viewport = useRef<HTMLDivElement>(null);
  const questionJump = useQuestionJump(target, viewport, questions.nonblocking);
  const { pinned, selectedText, focusedDiagram, isDiagramVisible, clear } =
    useHistorySelection(viewport);
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
  const [readingOutput, setReadingOutput] = useState<string>();
  const [foundOutput, setFoundOutput] = useState<{
    readonly rowId: string;
    readonly value: FoundOutput;
  }>();
  const [findAnchorEntry, setFindAnchorEntry] = useState<string>();
  const releaseFindAnchor = useCallback(() => setFindAnchorEntry(undefined), []);
  const [extraParts, setExtraParts] = useState<ExtraParts>();
  const partsRequest = useRef<AbortController | undefined>(undefined);
  const anchor = useRef<Position | undefined>(
    saved && !saved.following
      ? {
          id: saved.entryId,
          ...(saved.rowId ? { rowId: saved.rowId } : {}),
          offset: saved.offset,
          toolEntry: saved.toolEntry ?? true,
          reopenPages: 0,
        }
      : undefined,
  );
  const scrollIntent = useRef<"head" | "tail" | "scrollbar" | undefined>(undefined);
  const scrollbarTop = useRef<number | undefined>(undefined);
  const pageLead = useRef(160);
  const scrollGestures = useScrollGestures(gesture);
  const readingPosition = useRef<TimelineBookmark | undefined>(undefined);
  const written = useRef(saved);
  const requestedLatest = useRef(false);
  const activeLatest = useRef(false);
  const toolFocus = useRef<ReadTarget | undefined>(undefined);
  const renderedRows = useRef<readonly VisibleRow[]>([]);

  const history = useHistory(
    target,
    bookmark,
    following,
    pinned,
    focusedDiagram,
    isDiagramVisible,
    (position, wanted, extend) => {
      // Keep mounted group controls across a prepend or append that extends their native span.
      setGroupKeys(
        new Set(
          extend
            ? renderedRows.current
                .filter((row) => row.kind === "tool-run" || row.kind === "read-pack")
                .map((row) => row.id)
            : [],
        ),
      );
      if (position === "around" && wanted) {
        if (wanted.locator || anchor.current?.id !== wanted.entryId)
          anchor.current = {
            id: wanted.entryId,
            ...(wanted.rowId ? { rowId: wanted.rowId } : {}),
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
  const showWorking = working && Boolean(history.generation) && !history.newer;
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
      provider,
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
    provider,
  ]);
  useLayoutEffect(() => {
    renderedRows.current = entries;
  }, [entries]);

  const rangeExtractor = useCallback(
    (range: Parameters<typeof defaultRangeExtractor>[0]) =>
      transcriptRangeIndexes(range, entries, pinned, anchor.current, findAnchorEntry),
    [entries, pinned, findAnchorEntry],
  );

  const alignEnd = !child && !history.newer && (entries.length > 0 || showWorking);
  const fillCursor =
    alignEnd && following && !pinned.size && !history.loading && !history.waiting && !history.error
      ? (history.pages[0]?.nextCursor ?? undefined)
      : undefined;
  const { list, windowHeight, contentHeight } = useTimelineList({
    viewport,
    entries,
    rangeExtractor,
    anchorToStart: Boolean(focusedDiagram || (output && readingOutput === output.reference)),
    older: history.older,
    newer: history.newer,
    readingBelowPreview: (row) => !following && !anchor.current && !isPinnedRow(row, pinned),
    fillCursor,
    onFill: () => {
      if (anchor.current || scrollIntent.current) return false;
      void history.load("older", undefined, "prefill");
      return true;
    },
  });

  const prompts = useMemo(() => promptRanks(entries), [entries]);
  const virtualRows = list.getVirtualItems();
  const windowTop = following
    ? Math.max(0, contentHeight - windowHeight)
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

  useLayoutEffect(() => {
    if (!entries.length) return;
    if (
      requestedLatest.current ||
      (following && !anchor.current && !selectedText && !focusedDiagram)
    ) {
      list.scrollToEnd();
      requestedLatest.current = false;
      anchor.current = undefined;
    } else if (anchor.current) {
      const saved = anchor.current;
      const focus = toolFocus.current?.entryId === saved.id ? toolFocus.current : undefined;
      const exactEntry = Boolean(saved.rowId || focus || saved.toolEntry);
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
      const targetId = exactEntry ? targetRow.id : historyId(targetRow);
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
  }, [
    entries,
    following,
    list,
    prose,
    contentHeight,
    focusedDiagram,
    selectedText,
    showWorking,
    windowHeight,
  ]);

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

  function approachEdge(host: HTMLDivElement, direction: "head" | "tail") {
    const lead = pinned.size ? 160 : pageLead.current;
    if (direction === "head" && history.older && host.scrollTop < lead) {
      void history.load("older");
    } else if (
      direction === "tail" &&
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
    let intent = scrollIntent.current;
    if (intent === "scrollbar") {
      const previous = scrollbarTop.current;
      scrollbarTop.current = host.scrollTop;
      if (previous === undefined || previous === host.scrollTop) return;
      const up = host.scrollTop < previous;
      intent = up ? "head" : "tail";
      history.cancelOpposite(up);
      if (up) interruptLatest();
    }
    let nextFollowing = following;
    if (intent) {
      nextFollowing = intent !== "head" && nearEnd && !history.newer;
      approachEdge(host, intent);
      setFollowing(nextFollowing);
    }
    const position = historyBookmark(host, history.generation, nextFollowing);
    if (position) readingPosition.current = position;
  }

  function interruptLatest() {
    activeLatest.current = false;
    if (!following && !requestedLatest.current) return;
    requestedLatest.current = false;
    list.cancelScroll();
    setFollowing(false);
  }

  function gesture(up: boolean, distance?: number) {
    const host = viewport.current;
    if (!host || (up && host.scrollTop <= 0 && !history.older)) return;
    followBeforeFind.current = undefined;
    leaveReopen();
    scrollIntent.current = up ? "head" : "tail";
    pageLead.current = Math.max(
      160,
      Math.min(host.clientHeight * 3, (distance ?? host.clientHeight) * 3),
    );
    history.cancelOpposite(up);
    if (up) interruptLatest();
    approachEdge(host, up ? "head" : "tail");
  }

  function latest() {
    questionJump.cancel();
    setReadingOutput(undefined);
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

  async function moreParts(entryId: string, rowId: string, cursor: string) {
    const generation = history.generation;
    if (!generation || partsRequest.current) return;
    if (extraParts && extraParts.rowId !== rowId && pinned.has(extraParts.rowId)) {
      setActionNotice("Clear the active selection before opening another long entry.");
      return;
    }
    const prior =
      extraParts?.generation === generation && extraParts.rowId === rowId ? extraParts : undefined;
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
          rowId,
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
    const reference = output?.rowId === id ? output?.reference : undefined;
    if (!host || !row || !id || !reference || !host.contains(row)) return false;
    if (readingOutput === reference && !following && !requestedLatest.current) return false;
    setReadingOutput(reference);
    anchor.current = {
      id: row.dataset.nativeEntry ?? id,
      ...(row.dataset.questionRow ? { rowId: row.dataset.questionRow } : {}),
      toolEntry: true,
      offset: row.getBoundingClientRect().top - host.getBoundingClientRect().top,
    };
    scrollIntent.current = undefined;
    requestedLatest.current = false;
    list.cancelScroll();
    setFollowing(false);
    return true;
  }

  function openFound({ page, rowId, output: found, extra }: PreparedDestination) {
    const { entryId, generation, locator, contentRef, toolCallId } = found.destination;
    if (!history.openPrepared(page, { entryId, locator, ...(rowId !== entryId ? { rowId } : {}) }))
      return false;
    followBeforeFind.current = undefined;
    setFollowing(false);
    setFoundOutput({ rowId, value: found });
    setFindAnchorEntry(rowId);
    setOutput({ rowId, reference: contentRef });
    if (extra) setExtraParts({ rowId, generation, pages: 1, ...extra });
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
          questionJump.cancel();
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
      <HistoryStatus
        history={history}
        onRetry={() => void history.load("latest")}
        onReconnect={history.reconnect}
      />
      <div
        className="timeline-viewport"
        data-align-end={alignEnd ? "" : undefined}
        aria-label="Conversation history"
        role="region"
        ref={viewport}
        tabIndex={0}
        onScroll={recordScroll}
        {...scrollGestures}
        onPointerDown={(event) => {
          if (anchor.current?.adjacent) anchor.current = undefined;
          const box = event.currentTarget.getBoundingClientRect();
          if (event.target === event.currentTarget && box.right - event.clientX < 17) {
            followBeforeFind.current = undefined;
            leaveReopen();
            pageLead.current = event.currentTarget.clientHeight;
            scrollbarTop.current = event.currentTarget.scrollTop;
            scrollIntent.current = "scrollbar";
          }
        }}
        onKeyDown={(event) => {
          if (anchor.current?.adjacent) anchor.current = undefined;
          if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
          if (event.target instanceof Element && event.target.closest(".question-history")) return;
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
          <div className="timeline-spacer" style={{ height: contentHeight }}>
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
              const author = messageAuthor(row, child);
              const native = historyId(row);
              const found =
                foundOutput?.rowId === row.id &&
                foundOutput.value.destination.generation === history.generation
                  ? foundOutput.value
                  : undefined;
              return (
                <article
                  ref={list.measureElement}
                  data-index={item.index}
                  data-entry={row.id}
                  data-native-entry={row.kind === "delivery" ? undefined : native}
                  data-delivery-row={row.deliveryKey}
                  data-question-row={row.id !== native ? row.id : undefined}
                  data-live-entry={row.kind === "live" ? "" : undefined}
                  data-tool-member={row.grouped ? "" : undefined}
                  data-after-tools={followsTool && !justTools ? "" : undefined}
                  key={item.key}
                  className={`timeline-entry ${justTools ? "tool-entry" : author ? "user-entry" : "agent-entry"}`}
                  style={{ transform: `translateY(${item.start}px)` }}
                >
                  {!justTools && (
                    <HistoryEntryHeading row={row} author={author} provider={provider} />
                  )}
                  <div className="message-body stack">
                    {history.generation && (
                      <EntryBody
                        key={history.generation}
                        target={target}
                        generation={history.generation}
                        entry={row.entry}
                        delivery={row.delivery}
                        preferReceipt={provider === "codex"}
                        provisional={row.provisional}
                        knownTools={knownTools}
                        expanded={expanded}
                        prose={prose}
                        questions={questions}
                        observeQuestion={questionJump.observe}
                        toggle={(key, open) => setExpanded((known) => toggleKey(known, key, open))}
                        reference={output?.rowId === row.id ? output.reference : undefined}
                        {...(extraParts?.generation === history.generation &&
                        extraParts.rowId === row.id
                          ? { extra: extraParts }
                          : {})}
                        onMoreParts={(cursor) => void moreParts(native, row.id, cursor)}
                        onReadRange={holdOutputRange}
                        {...(found ? { found } : {})}
                        onReference={(reference) => {
                          setFoundOutput(undefined);
                          setReadingOutput(undefined);
                          setOutput(reference ? { rowId: row.id, reference } : undefined);
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
        {showWorking && (
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
            questionJump.cancel();
            followBeforeFind.current = undefined;
            leaveReopen();
            setFollowing(false);
            void history.load("around", { entryId: id, locator });
          }}
        />
      )}
      <TranscriptJumpControls
        jump={questionJump}
        questions={questions}
        latestLabel={
          history.waiting ? "New messages available" : !following ? "Jump to latest" : undefined
        }
        onLatest={latest}
        onChoose={({ page, ...wanted }) => {
          if (!history.openPrepared(page, wanted)) return false;
          followBeforeFind.current = undefined;
          requestedLatest.current = false;
          setFollowing(false);
          return true;
        }}
        onReveal={() => {
          leaveReopen();
          list.cancelScroll();
          scrollIntent.current = undefined;
        }}
        onError={setActionNotice}
      />
    </section>
  );
}
