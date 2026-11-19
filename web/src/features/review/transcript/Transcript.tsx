import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import {
  useCallback,
  useDeferredValue,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { HistoryEntry, HistoryPart } from "../../../api/generated/contracts";
import type { HostProviderDto } from "../../../api/generated/contracts/common";
import type { MessageHistoryItem } from "../../../api/generated/contracts/delivery";
import { historyParts } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Icon } from "../../../components/Icon";
import { Empty, Loading, Notice } from "../../../components/PanelState";
import { describeError, formatClockTime, formatTime, readable } from "../../../lib/presentation";
import { EntryBody } from "./EntryBody";
import { entriesIn, readBookmark, rememberBookmark } from "./history-window";
import { PromptNavigator } from "./PromptNavigator";
import { FileReadPack, ToolRunHeader } from "./ToolActivity";
import { activityRows, openKey, toolCatalog, toolOnly } from "./tool-activity";
import type { ReadPack, ReadTarget, ToolRun } from "./tool-activity";
import { MarkdownContent } from "../../../components/markdown/MarkdownContent";
import { useHistory } from "./useHistory";

type Position = {
  readonly id: string;
  readonly offset: number;
  readonly toolEntry?: boolean;
  readonly callId?: string;
};
type ExtraParts = {
  readonly entryId: string;
  readonly generation: string;
  readonly pages: number;
  readonly items: readonly HistoryPart[];
  readonly next: string | null;
};
type OutputSelection = { readonly entryId: string; readonly reference: string };
type MessageRow = {
  readonly kind: "native" | "live" | "delivery";
  readonly id: string;
  readonly entry?: HistoryEntry;
  readonly delivery?: MessageHistoryItem;
  readonly provisional?: string;
  readonly grouped?: boolean;
};
type Visible = MessageRow | ToolRun | ReadPack;
type Props = {
  readonly target: SessionAddress;
  readonly provider: HostProviderDto;
  readonly child: boolean;
  readonly messages: readonly MessageHistoryItem[];
  readonly resultOnly: boolean;
  readonly working: boolean;
  readonly onObservedTurn: (turn: string | undefined) => void;
};

export function Transcript({
  target,
  provider,
  child,
  messages,
  resultOnly,
  working,
  onObservedTurn,
}: Props) {
  const providerName = readable(provider);
  const bookmark = `history.${target.runId}.${target.sessionKey}`;
  const [saved] = useState(() => readBookmark(bookmark));
  const [following, setFollowing] = useState(saved?.following ?? true);
  const [pinned, setPinned] = useState<ReadonlySet<string>>(() => new Set());
  const [selectedText, setSelectedText] = useState(false);
  const [actionNotice, setActionNotice] = useState<string>();
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(
    () =>
      new Set(
        saved && !saved.following && saved.callId ? [openKey(saved.entryId, saved.callId)] : [],
      ),
  );
  const [expandedRuns, setExpandedRuns] = useState<ReadonlySet<string>>(
    () => new Set(saved && !saved.following && saved.toolEntry !== false ? [saved.entryId] : []),
  );
  const [expandedReads, setExpandedReads] = useState<ReadonlySet<string>>(
    () => new Set(saved && !saved.following && saved.toolEntry !== false ? [saved.entryId] : []),
  );
  const [output, setOutput] = useState<OutputSelection>();
  const [extraParts, setExtraParts] = useState<ExtraParts>();
  const partsRequest = useRef<AbortController | undefined>(undefined);
  const viewport = useRef<HTMLDivElement>(null);
  const anchor = useRef<Position | undefined>(
    saved
      ? { id: saved.entryId, offset: saved.offset, toolEntry: saved.toolEntry ?? true }
      : undefined,
  );
  const selectionState = useRef({ hasText: false, ids: new Set<string>() });
  const scrollIntent = useRef(false);
  const requestedLatest = useRef(false);
  const activeLatest = useRef(false);
  const toolFocus = useRef<ReadTarget | undefined>(undefined);

  const history = useHistory(target, bookmark, following, pinned, (position, wanted) => {
    if (position === "around" && wanted) {
      if (anchor.current?.id !== wanted.entryId) anchor.current = { id: wanted.entryId, offset: 0 };
    } else if (position !== "latest") {
      anchor.current = visibleAnchor(viewport.current);
    }
  });
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
  const clippedStart = Boolean(history.older);
  const clippedEnd = Boolean(history.newer || inProgress);
  const entries = useMemo<Visible[]>(() => {
    if (resultOnly) {
      const result = [...historyEntries]
        .reverse()
        .find((entry) => entry.parts.some((part) => part.kind === "assistant_text"));
      return result ? [{ kind: "native", id: result.entryId, entry: result }] : [];
    }
    const deliveries = new Map(
      messages.flatMap((delivery) =>
        delivery.nativeEntryId ? [[delivery.nativeEntryId, delivery] as const] : [],
      ),
    );
    const rows: Visible[] = activityRows(
      historyEntries,
      knownTools,
      expandedRuns,
      expandedReads,
      clippedStart,
      clippedEnd,
    ).map((row) => {
      if (row.kind !== "native") return row;
      const delivery = deliveries.get(row.id);
      return delivery ? { ...row, delivery } : row;
    });
    if (following && inProgress)
      rows.push({ kind: "live", id: inProgress.entryId, provisional: inProgress.text });
    for (const delivery of messages.slice().reverse()) {
      if (delivery.nativeEntryId && canonical.has(delivery.nativeEntryId)) continue;
      rows.push({ kind: "delivery", id: `delivery:${delivery.messageId}`, delivery });
    }
    return rows;
  }, [
    canonical,
    clippedEnd,
    clippedStart,
    expandedReads,
    expandedRuns,
    following,
    historyEntries,
    inProgress,
    knownTools,
    messages,
    resultOnly,
  ]);

  const rangeExtractor = useCallback(
    (range: Parameters<typeof defaultRangeExtractor>[0]) => {
      const indexes = new Set(defaultRangeExtractor(range));
      if (pinned.size)
        entries.forEach((entry, index) => {
          if (pinned.has(historyId(entry))) indexes.add(index);
        });
      return [...indexes].sort((left, right) => left - right);
    },
    [entries, pinned],
  );

  // Keep scroll measurements and the TanStack instance within this component.
  // eslint-disable-next-line react-hooks/incompatible-library
  const list = useVirtualizer({
    count: entries.length,
    getScrollElement: () => viewport.current,
    getItemKey: (index) => entries[index]?.id ?? index,
    estimateSize: (index) =>
      entries[index]?.kind === "tool-run" ? 35 : entries[index]?.kind === "read-pack" ? 55 : 95,
    overscan: 7,
    rangeExtractor,
  });

  useLayoutEffect(() => {
    if (!entries.length) return;
    if (following || requestedLatest.current) {
      list.scrollToIndex(entries.length - 1, { align: "end" });
      requestedLatest.current = false;
      anchor.current = undefined;
    } else if (anchor.current) {
      const saved = anchor.current;
      const focus = toolFocus.current?.entryId === saved.id ? toolFocus.current : undefined;
      const exactEntry = Boolean(focus || saved.toolEntry);
      const index = entries.findIndex(
        (entry) => (exactEntry ? entry.id : historyId(entry)) === saved.id,
      );
      if (index < 0) return;
      const container = viewport.current;
      list.scrollToIndex(index, { align: "start" });
      requestAnimationFrame(() => {
        if (!container?.isConnected) return;
        const displayed = [...container.querySelectorAll<HTMLElement>("[data-entry]")];
        const row = displayed.find((item) =>
          exactEntry ? item.dataset.entry === saved.id : item.dataset.nativeEntry === saved.id,
        );
        if (row)
          container.scrollTop +=
            row.getBoundingClientRect().top - container.getBoundingClientRect().top - saved.offset;
        if (focus && toolFocus.current === focus) {
          const tool = [...(row?.querySelectorAll<HTMLElement>("[data-tool-call]") ?? [])].find(
            (item) => item.dataset.toolCall === focus.callId,
          );
          (tool ?? container).focus({ preventScroll: true });
          toolFocus.current = undefined;
        }
      });
      anchor.current = undefined;
    }
  }, [entries, following, list]);

  useEffect(() => {
    onObservedTurn(history.live?.turnId);
  }, [history.live?.turnId, onObservedTurn]);
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
      const focused = document.activeElement?.closest<HTMLElement>("[data-native-entry]");
      if (focused?.dataset.nativeEntry && host.contains(focused))
        ids.add(focused.dataset.nativeEntry);
      const previous = selectionState.current;
      if (
        previous.hasText !== hasText ||
        previous.ids.size !== ids.size ||
        [...ids].some((id) => !previous.ids.has(id))
      ) {
        selectionState.current = { hasText, ids };
        setPinned(ids);
        setSelectedText(hasText);
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

  useEffect(() => {
    const host = viewport.current;
    function save() {
      const current = visibleAnchor(host);
      if (current && history.generation)
        rememberBookmark(bookmark, {
          entryId: current.id,
          offset: current.offset,
          following,
          generation: history.generation,
          toolEntry: current.toolEntry ?? false,
          ...(current.callId ? { callId: current.callId } : {}),
        });
    }
    window.addEventListener("pagehide", save);
    return () => {
      save();
      window.removeEventListener("pagehide", save);
    };
  }, [bookmark, following, history.generation]);

  function recordScroll() {
    const host = viewport.current;
    if (!host) return;
    const nearEnd = host.scrollHeight - host.clientHeight - host.scrollTop < 42;
    if (scrollIntent.current) {
      setFollowing(nearEnd);
      scrollIntent.current = false;
    }
    const current = visibleAnchor(host);
    if (current && history.generation)
      rememberBookmark(bookmark, {
        entryId: current.id,
        offset: current.offset,
        following: nearEnd,
        generation: history.generation,
        toolEntry: current.toolEntry ?? false,
        ...(current.callId ? { callId: current.callId } : {}),
      });
  }

  function gesture(up: boolean) {
    scrollIntent.current = true;
    const host = viewport.current;
    if (!host || history.loading) return;
    if (up && host.scrollTop < 2 && history.older) {
      setFollowing(false);
      void history.load("older");
    } else if (!up && host.scrollHeight - host.clientHeight - host.scrollTop < 2 && history.newer) {
      void history.load("newer");
    }
  }

  function clear() {
    if (selectedText) getSelection()?.removeAllRanges();
    setSelectedText(false);
    setPinned(new Set());
    selectionState.current = { hasText: false, ids: new Set() };
  }

  function latest() {
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
    anchor.current = visibleAnchor(viewport.current);
    setFollowing(false);
    setExpandedRuns((known) => {
      if (!run.open) return toggleKey(known, run.anchorId, true);
      const next = new Set(known);
      for (const id of run.members) next.delete(id);
      return next;
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
    setFollowing(false);
    setExpandedReads((known) => toggleKey(known, at.entryId, true));
    setExpanded((known) => toggleKey(known, openKey(at.entryId, at.callId), true));
  }

  return (
    <section
      className="transcript-shell"
      aria-label={resultOnly ? "Latest child output" : "Native conversation"}
    >
      {!resultOnly && (
        <div className={`timeline-controls ${selectedText ? "has-selection" : ""}`}>
          {history.older && (
            <button
              type="button"
              disabled={!history.older || history.loading}
              onClick={() => {
                setFollowing(false);
                void history.load("older");
              }}
            >
              Earlier
            </button>
          )}
          {history.newer && (
            <button
              type="button"
              disabled={!history.newer || history.loading}
              onClick={() => void history.load("newer")}
            >
              Later
            </button>
          )}
          {selectedText && (
            <>
              <button
                type="button"
                onClick={() => {
                  const value = getSelection()?.toString();
                  if (value)
                    void navigator.clipboard
                      .writeText(value)
                      .catch((failure: unknown) => setActionNotice(describeError(failure)));
                }}
              >
                Copy selection
              </button>
              <button type="button" onClick={clear}>
                Clear selection
              </button>
            </>
          )}
        </div>
      )}
      {actionNotice && <Notice>{actionNotice}</Notice>}
      {history.error && (
        <Notice retry={() => void history.load("latest")}>
          {history.error}
          {history.updated && ` · Last confirmed ${formatTime(history.updated)}`}
        </Notice>
      )}
      {!history.online && (
        <Notice>
          Live updates paused
          {history.updated ? ` · Last confirmed ${formatTime(history.updated)}` : ""}.
        </Notice>
      )}
      {history.availability && history.availability !== "available" && (
        <Notice>
          {history.availability === "partial"
            ? "Only part of this conversation is available."
            : `Native history · ${readable(history.availability)}`}
        </Notice>
      )}
      {resultOnly && (
        <p className="timeline-live meta">Latest assistant output from this child session</p>
      )}
      <div
        className="timeline-viewport"
        ref={viewport}
        tabIndex={0}
        onScroll={recordScroll}
        onWheel={(event) => {
          if (event.deltaY) gesture(event.deltaY < 0);
        }}
        onTouchMove={() => {
          scrollIntent.current = true;
        }}
        onPointerDown={(event) => {
          const box = event.currentTarget.getBoundingClientRect();
          if (event.target === event.currentTarget && box.right - event.clientX < 17)
            scrollIntent.current = true;
        }}
        onKeyDown={(event) => {
          if (["PageUp", "ArrowUp", "Home"].includes(event.key)) gesture(true);
          else if (["PageDown", "ArrowDown", "End", " "].includes(event.key)) gesture(false);
        }}
      >
        {!entries.length &&
          (history.loading ? (
            <Loading label="Reading native conversation…" />
          ) : (
            !history.error && (
              <Empty
                title={resultOnly ? "No assistant output recorded" : "No conversation entries"}
              />
            )
          ))}
        {entries.length > 0 && (
          <div className="timeline-spacer" style={{ height: list.getTotalSize() }}>
            {list.getVirtualItems().map((item) => {
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
              return (
                <article
                  ref={list.measureElement}
                  data-index={item.index}
                  data-entry={row.id}
                  data-native-entry={row.kind === "delivery" ? undefined : row.id}
                  data-tool-member={row.grouped ? "" : undefined}
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
                        <time dateTime={timestamp} title={formatTime(timestamp)}>
                          {formatClockTime(timestamp)}
                        </time>
                      )}
                      {row.delivery && (
                        <span className="badge">{readable(row.delivery.state)}</span>
                      )}
                      {row.provisional !== undefined && (
                        <span className="badge green">Working</span>
                      )}
                    </header>
                  )}
                  <div className="message-body stack">
                    {row.provisional !== undefined && <MarkdownContent source={row.provisional} />}
                    {row.delivery && !row.entry && <MarkdownContent source={row.delivery.text} />}
                    {row.entry && history.generation && (
                      <EntryBody
                        key={history.generation}
                        target={target}
                        generation={history.generation}
                        entry={row.entry}
                        knownTools={knownTools}
                        expanded={expanded}
                        toggle={(key, open) => setExpanded((known) => toggleKey(known, key, open))}
                        reference={output?.entryId === row.id ? output.reference : undefined}
                        {...(extraParts?.generation === history.generation &&
                        extraParts.entryId === row.id
                          ? { extra: extraParts }
                          : {})}
                        onMoreParts={(cursor) => void moreParts(row.id, cursor)}
                        onReference={(reference) =>
                          setOutput(reference ? { entryId: row.id, reference } : undefined)
                        }
                      />
                    )}
                  </div>
                </article>
              );
            })}
          </div>
        )}
        {!resultOnly && working && !inProgress && (
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
      {!resultOnly && history.generation && (
        <PromptNavigator
          key={history.generation}
          target={target}
          generation={history.generation}
          following={following}
          onReturnFocus={() => viewport.current?.focus({ preventScroll: true })}
          onJump={(id, locator) => {
            setFollowing(false);
            void history.load("around", { entryId: id, locator });
          }}
        />
      )}
      {!resultOnly && (!following || history.waiting) && (
        <button type="button" className="timeline-jump" onClick={latest}>
          <span aria-hidden="true">↓ </span>
          {history.waiting ? "New messages available" : "Jump to latest"}
        </button>
      )}
    </section>
  );
}

function ProviderMark({ provider }: { readonly provider: HostProviderDto }) {
  if (provider === "codex") return <Icon name="terminal" size={14} />;
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      aria-hidden="true"
    >
      <path d="m12 2 1.5 6.5L19 5l-3.5 5.5L22 12l-6.5 1.5L19 19l-5.5-3.5L12 22l-1.5-6.5L5 19l3.5-5.5L2 12l6.5-1.5L5 5l5.5 3.5Z" />
    </svg>
  );
}

function visibleAnchor(host: HTMLDivElement | null): Position | undefined {
  if (!host) return;
  const frame = host.getBoundingClientRect();
  const active = document.activeElement?.closest<HTMLElement>("[data-tool-call]");
  const focused = active?.closest<HTMLElement>("[data-native-entry]");
  if (focused && host.contains(focused) && focused.dataset.nativeEntry) {
    const bounds = focused.getBoundingClientRect();
    if (bounds.bottom > frame.top && bounds.top < frame.bottom)
      return {
        id: focused.dataset.nativeEntry,
        offset: bounds.top - frame.top,
        toolEntry: true,
        ...(active?.dataset.toolCall ? { callId: active.dataset.toolCall } : {}),
      };
  }
  for (const node of host.querySelectorAll<HTMLElement>("[data-native-entry]")) {
    const item = node.getBoundingClientRect();
    if (item.bottom > frame.top && node.dataset.nativeEntry) {
      const tool = node.querySelector<HTMLElement>("details[open] > [data-tool-call]");
      return {
        id: node.dataset.nativeEntry,
        offset: item.top - frame.top,
        toolEntry:
          node.classList.contains("tool-entry") && node.dataset.entry === node.dataset.nativeEntry,
        ...(tool?.dataset.toolCall ? { callId: tool.dataset.toolCall } : {}),
      };
    }
  }
}

function historyId(row: Visible) {
  return "anchorId" in row ? row.anchorId : row.id;
}

function toggleKey(known: ReadonlySet<string>, key: string, open: boolean) {
  if (known.has(key) === open) return known;
  const next = new Set(known);
  if (open) next.add(key);
  else next.delete(key);
  return next.size > 160 ? new Set([...next].slice(-160)) : next;
}
