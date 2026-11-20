import { useEffect, useEffectEvent, useRef, useState } from "react";
import type { HistoryNavigation } from "../../../api/generated/contracts/history";
import type { PromptGroup, PromptPreview } from "../../../api/generated/contracts/history";
import { historyPrompts } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { HistoryTimestamp } from "./HistoryTimestamp";
import "./prompt-navigator.css";

type PreviewPath = { readonly group: string; readonly pages?: readonly string[] };
type PreviewPage = Extract<HistoryNavigation, { kind: "previews" }>;
const MAX_PREVIEW_ITEMS = 24;
const MAX_CACHED_PREVIEWS = 2;
const MARKER_HEIGHT = 12;
const RAIL_INSETS = 28;

function fitPromptGroups(groups: readonly PromptGroup[], capacity: number) {
  if (groups.length <= capacity) return groups;
  const fitted: PromptGroup[] = [];
  for (let i = 0; i < capacity; i++) {
    const first = groups[Math.floor((i * groups.length) / capacity)]!;
    const last = groups[Math.floor(((i + 1) * groups.length) / capacity) - 1]!;
    // The first group's signed cursor can page through the combined ordinal range.
    fitted.push({ ...first, last: last.last });
  }
  return fitted;
}

export function PromptNavigator({
  target,
  generation,
  following,
  rank,
  promptOffsets,
  windowCount,
  hasOlder,
  hasNewer,
  onJump,
  onReturnFocus,
}: {
  readonly target: SessionAddress;
  readonly generation: string;
  readonly following: boolean;
  readonly rank: number;
  readonly promptOffsets: ReadonlyMap<string, number>;
  readonly windowCount: number;
  readonly hasOlder: boolean;
  readonly hasNewer: boolean;
  readonly onJump: (id: string, locator: string) => void;
  readonly onReturnFocus: () => void;
}) {
  const rail = useRef<HTMLDivElement>(null);
  const previewCache = useRef(new Map<string, PreviewPage>());
  const [capacity, setCapacity] = useState(1);
  const [activation, setActivation] = useState<string>();
  const [visible, setVisible] = useState(false);
  const [pinned, setPinned] = useState(false);
  const [jumped, setJumped] = useState<Pick<PromptPreview, "entryId" | "ordinal">>();
  const [windows, setWindows] = useState<readonly string[]>([]);
  const [selected, setSelected] = useState<PreviewPath>();
  useEffect(() => {
    const host = rail.current;
    if (!host) return;
    let previous = 0;
    const observer = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const count = Math.max(
        1,
        Math.min(64, Math.floor((entry.contentRect.height - RAIL_INSETS) / MARKER_HEIGHT)),
      );
      if (previous === count) return;
      previous = count;
      setCapacity(count);
      setSelected(undefined);
      setActivation(undefined);
    });
    observer.observe(host);
    return () => observer.disconnect();
  }, []);

  const scope = `${target.runId}:${target.sessionKey}:${generation}`;
  const overviewCursor = windows.at(-1);
  const overview = useResource(
    `prompts:${scope}:${visible}:${overviewCursor ?? ""}`,
    async (signal) => {
      if (!visible) return { data: null, cursor: "" };
      // The default overview uses the service's verified sparse index at every rail size.
      const result = await historyPrompts(target, overviewCursor ? { overviewCursor } : {}, signal);
      if (result.data.generation !== generation) throw new Error("Message positions changed.");
      return result;
    },
    visible ? [{ kind: "history", id: target.sessionKey }] : [],
  );
  const overviewPage = overview.data?.kind === "overview" ? overview.data : undefined;
  const groups = fitPromptGroups(overviewPage?.groups ?? [], capacity);
  const firstOrdinal = groups[0]?.first ?? 1;
  const shownCount = (groups.at(-1)?.last ?? firstOrdinal) - firstOrdinal + 1;
  const jumpRank = jumped && promptOffsets.get(jumped.entryId);
  const base = !hasOlder
    ? 0
    : !hasNewer && overviewPage?.complete
      ? overviewPage.total - windowCount
      : jumped && jumpRank !== undefined
        ? jumped.ordinal - jumpRank
        : undefined;
  const location =
    following && overviewPage?.complete
      ? overviewPage.total
      : base !== undefined
        ? base + rank
        : undefined;
  const current = groups.find(
    (group) => location !== undefined && group.first <= location && location <= group.last,
  );
  const group =
    groups.find((item) => item.cursor === selected?.group) ??
    (pinned ? (current ?? groups.at(-1)) : undefined);
  // An empty saved trail keeps the pager mounted while returning to its first page.
  const pageTrail = group?.cursor === selected?.group ? selected?.pages : undefined;
  const previewPages = pageTrail ?? [];
  const previewCursor = previewPages.at(-1) ?? group?.cursor;
  const limit = group ? Math.min(MAX_PREVIEW_ITEMS, group.last - group.first + 1) : 0;
  const previewKey = visible && previewCursor ? `${limit}:${previewCursor}` : "";
  const previews = useResource(`prompt-preview:${scope}:${previewKey}`, async (signal) => {
    if (!previewKey || !previewCursor) return { data: null, cursor: "" };
    const cached = previewCache.current.get(previewKey);
    if (cached) return { data: cached, cursor: "" };
    const result = await historyPrompts(target, { cursor: previewCursor, limit }, signal);
    signal.throwIfAborted();
    if (
      result.data.generation !== generation ||
      result.data.reset ||
      result.data.kind !== "previews"
    )
      throw new Error("Message positions changed. Reopen the conversation to refresh them.");
    const cache = previewCache.current;
    cache.set(previewKey, result.data);
    if (cache.size > MAX_CACHED_PREVIEWS) {
      const oldest = cache.keys().next().value;
      if (oldest !== undefined) cache.delete(oldest);
    }
    return { data: result.data, cursor: result.cursor };
  });
  const page = previews.data?.kind === "previews" ? previews.data : undefined;
  const items = page?.items.filter(
    (item) => group && group.first <= item.ordinal && item.ordinal <= group.last,
  );
  const last = items?.at(-1);
  const nextPreview = group && (!last || last.ordinal < group.last) ? page?.nextCursor : null;
  const canGoBack = !previews.pending && previewPages.length > 0;
  const empty = overviewPage?.total === 0 && overviewPage.complete;

  function close(restoreFocus = false) {
    setSelected(undefined);
    setActivation(undefined);
    setPinned(false);
    setVisible(false);
    if (restoreFocus) onReturnFocus();
  }

  function open() {
    setVisible(true);
  }

  function choose(group: PromptGroup) {
    if (selected?.group !== group.cursor) {
      setActivation(undefined);
      setSelected({ group: group.cursor });
    }
    open();
  }

  function jump(item: PromptPreview) {
    setJumped({ entryId: item.entryId, ordinal: item.ordinal });
    onJump(item.entryId, item.locator);
    close(true);
  }

  function activate(group: PromptGroup) {
    choose(group);
    setPinned(true);
    if (group.first !== group.last) return;
    const shown = items?.find((item) => item.ordinal === group.first);
    if (shown) jump(shown);
    else setActivation(group.cursor);
  }

  const finishActivation = useEffectEvent(() => {
    if (activation && group?.cursor === activation && group.first === group.last) {
      const item = items?.find((candidate) => candidate.ordinal === group.first);
      if (item) jump(item);
    }
  });
  useEffect(() => {
    if (!activation || !page) return;
    const frame = requestAnimationFrame(() => finishActivation());
    return () => cancelAnimationFrame(frame);
  }, [activation, page]);

  return (
    <div
      className="message-navigator"
      data-visible={visible && !empty}
      ref={rail}
      onPointerEnter={(event) => {
        if (event.pointerType !== "touch") open();
      }}
      onPointerLeave={() => {
        if (!pinned && !rail.current?.contains(document.activeElement)) close();
      }}
      onFocusCapture={open}
      onBlurCapture={(event) => {
        if (!pinned && !event.currentTarget.contains(event.relatedTarget)) close();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          event.stopPropagation();
          close(true);
          return;
        }
        if (
          !(event.target instanceof HTMLButtonElement) ||
          !event.target.hasAttribute("data-navigation-marker")
        )
          return;
        if (!["ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
        event.preventDefault();
        const markers = [
          ...event.currentTarget.querySelectorAll<HTMLButtonElement>("[data-navigation-marker]"),
        ];
        const index = markers.indexOf(event.target);
        const move = event.key === "ArrowUp" ? -1 : 1;
        const next =
          event.key === "Home" ? 0 : event.key === "End" ? markers.length - 1 : index + move;
        markers[Math.max(0, Math.min(markers.length - 1, next))]?.focus({ preventScroll: true });
      }}
    >
      <button
        type="button"
        className="message-navigator-reveal"
        aria-label="Navigate your messages"
        aria-expanded={visible && !empty}
        hidden={empty}
        onClick={() => {
          if (pinned) close(true);
          else {
            const initial = current ?? groups.at(-1);
            if (initial) choose(initial);
            setPinned(true);
            open();
          }
        }}
      >
        <span aria-hidden="true">⋮</span>
      </button>
      {groups.length > 0 && (
        <div className="message-navigator-rail" role="group" aria-label="Your messages">
          {groups.map((marker) => (
            <button
              key={marker.cursor}
              type="button"
              className="message-navigator-marker"
              data-navigation-marker=""
              aria-current={current?.cursor === marker.cursor ? "location" : undefined}
              aria-label={
                marker.first === marker.last
                  ? `Message ${marker.first}`
                  : `Messages ${marker.first} to ${marker.last}`
              }
              style={{
                height: `min(${MARKER_HEIGHT}px, ${((marker.last - marker.first + 1) / shownCount) * 100}%)`,
                top: `${((marker.first - firstOrdinal + (marker.last - marker.first + 1) / 2) / shownCount) * 100}%`,
              }}
              tabIndex={visible ? 0 : -1}
              onPointerEnter={() => choose(marker)}
              onFocus={() => choose(marker)}
              onClick={() => void activate(marker)}
            >
              <span aria-hidden="true" />
            </button>
          ))}
        </div>
      )}
      {visible &&
        !empty &&
        (group ||
          overview.pending ||
          overview.error ||
          overviewPage?.nextWindowCursor ||
          windows.length > 0) && (
          <div className="message-navigator-preview" aria-label="Message previews">
            <div className="message-navigator-heading">
              <span>
                {group
                  ? group.first === group.last
                    ? "Your message"
                    : `${group.last - group.first + 1} messages`
                  : "Your messages"}
              </span>
              <button
                type="button"
                className="quiet-button"
                aria-label="Close message navigator"
                onClick={() => close(true)}
              >
                ×
              </button>
            </div>
            {overview.error && <Notice retry={overview.retry}>{overview.error}</Notice>}
            {previews.error && <Notice retry={previews.retry}>{previews.error}</Notice>}
            {(overview.pending || (group && previews.pending)) && (
              <Loading label="Reading messages…" />
            )}
            {items && items.length > 0 && (
              <ol className="message-navigator-items">
                {items.map((item) => (
                  <li key={item.entryId}>
                    <button type="button" onClick={() => jump(item)}>
                      <span>{item.text || "User message"}</span>
                      {item.timestamp && (
                        <HistoryTimestamp timestamp={item.timestamp} includeDate />
                      )}
                    </button>
                  </li>
                ))}
              </ol>
            )}
            {(pageTrail !== undefined || nextPreview) && group && (
              <div className="message-navigator-paging">
                <button
                  type="button"
                  className="quiet-button"
                  aria-disabled={!canGoBack}
                  onClick={() => canGoBack && setSelected({ group: group.cursor, pages: [] })}
                >
                  First
                </button>
                <button
                  type="button"
                  className="quiet-button"
                  aria-disabled={!canGoBack}
                  onClick={() =>
                    canGoBack &&
                    setSelected({ group: group.cursor, pages: previewPages.slice(0, -1) })
                  }
                >
                  Previous
                </button>
                <button
                  type="button"
                  className="quiet-button"
                  aria-disabled={!nextPreview}
                  onClick={() =>
                    nextPreview &&
                    setSelected({ group: group.cursor, pages: [...previewPages, nextPreview] })
                  }
                >
                  Next
                </button>
              </div>
            )}
            {(overviewPage?.nextWindowCursor || windows.length > 0) && (
              <div className="message-navigator-paging">
                {windows.length > 0 && (
                  <button
                    type="button"
                    className="quiet-button"
                    onClick={() => setWindows(windows.slice(0, -1))}
                  >
                    Earlier groups
                  </button>
                )}
                {overviewPage?.nextWindowCursor && (
                  <button
                    type="button"
                    className="quiet-button"
                    onClick={() =>
                      overviewPage.nextWindowCursor &&
                      setWindows([...windows, overviewPage.nextWindowCursor])
                    }
                  >
                    More groups
                  </button>
                )}
              </div>
            )}
            {overviewPage && !overviewPage.complete && !overviewPage.nextWindowCursor && (
              <p className="meta">Some messages are unavailable.</p>
            )}
          </div>
        )}
    </div>
  );
}
