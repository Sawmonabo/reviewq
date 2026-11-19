import { useEffect, useRef, useState } from "react";
import type { PromptGroup, PromptPreview } from "../../../api/generated/contracts/history";
import { historyPrompts } from "../../../api/history";
import type { SessionAddress } from "../../../api/history";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { describeError } from "../../../lib/presentation";
import { HistoryTimestamp } from "./HistoryTimestamp";
import "./prompt-navigator.css";

type PreviewPath = { readonly group: string; readonly pages: readonly string[] };

export function PromptNavigator({
  target,
  generation,
  following,
  onJump,
  onReturnFocus,
}: {
  readonly target: SessionAddress;
  readonly generation: string;
  readonly following: boolean;
  readonly onJump: (id: string, locator: string) => void;
  readonly onReturnFocus: () => void;
}) {
  const rail = useRef<HTMLDivElement>(null);
  const activation = useRef<AbortController | undefined>(undefined);
  const [maxGroups, setMaxGroups] = useState(32);
  const [visible, setVisible] = useState(false);
  const [pinned, setPinned] = useState(false);
  const [activeOrdinal, setActiveOrdinal] = useState<number>();
  const [windows, setWindows] = useState<readonly string[]>([]);
  const [selected, setSelected] = useState<PreviewPath>();
  const [jumpError, setJumpError] = useState<string>();

  useEffect(() => {
    const element = rail.current;
    if (!element) return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) setMaxGroups(Math.max(1, Math.min(64, Math.floor(entry.contentRect.height / 12))));
    });
    observer.observe(element);
    return () => {
      activation.current?.abort();
      observer.disconnect();
    };
  }, []);

  const scope = `${target.runId}:${target.sessionKey}:${generation}`;
  const overviewCursor = windows.at(-1);
  const overview = useResource(
    `prompts:${scope}:${visible}:${maxGroups}:${overviewCursor ?? ""}`,
    (signal) =>
      visible
        ? historyPrompts(
            target,
            { maxGroups, ...(overviewCursor ? { overviewCursor } : {}) },
            signal,
          )
        : Promise.resolve({ data: null, cursor: "" }),
    visible ? [{ kind: "history", id: target.sessionKey }] : [],
  );
  const window = overview.data?.kind === "overview" ? overview.data : undefined;
  const groups = window?.groups ?? [];
  const location = following && window?.complete ? window.total : activeOrdinal;
  const current = groups.find(
    (group) => location !== undefined && group.first <= location && location <= group.last,
  );
  const group =
    groups.find((item) => item.cursor === selected?.group) ??
    (pinned ? (current ?? groups.at(-1)) : undefined);
  const previewPages = group?.cursor === selected?.group ? (selected?.pages ?? []) : [];
  const previewCursor = previewPages.at(-1) ?? group?.cursor;
  const previews = useResource(
    `prompt-preview:${scope}:${visible}:${previewCursor ?? ""}`,
    (signal) =>
      visible && previewCursor
        ? historyPrompts(target, { cursor: previewCursor, limit: 24 }, signal)
        : Promise.resolve({ data: null, cursor: "" }),
  );
  const page = previews.data?.kind === "previews" ? previews.data : undefined;
  const items = page?.items.filter(
    (item) => group && group.first <= item.ordinal && item.ordinal <= group.last,
  );
  const last = items?.at(-1);
  const nextPreview = group && (!last || last.ordinal < group.last) ? page?.nextCursor : null;
  const empty = window?.total === 0 && window.complete;

  function close(restoreFocus = false) {
    activation.current?.abort();
    setPinned(false);
    setVisible(false);
    if (restoreFocus) onReturnFocus();
  }

  function choose(group: PromptGroup) {
    if (selected?.group !== group.cursor) {
      activation.current?.abort();
      setSelected({ group: group.cursor, pages: [] });
      setJumpError(undefined);
    }
    setVisible(true);
  }

  function jump(item: PromptPreview) {
    setActiveOrdinal(item.ordinal);
    onJump(item.entryId, item.locator);
    close(true);
  }

  async function activate(group: PromptGroup) {
    choose(group);
    setPinned(true);
    if (group.first !== group.last) return;
    const shown = items?.find((item) => item.ordinal === group.first);
    if (shown) {
      jump(shown);
      return;
    }
    activation.current?.abort();
    const controller = new AbortController();
    activation.current = controller;
    try {
      const { data } = await historyPrompts(
        target,
        { cursor: group.cursor, limit: 1 },
        controller.signal,
      );
      if (controller.signal.aborted || data.kind !== "previews" || data.generation !== generation)
        return;
      const exact = data.items.find((item) => item.ordinal === group.first);
      if (exact) jump(exact);
    } catch (error) {
      if (!controller.signal.aborted) setJumpError(describeError(error));
    }
  }

  return (
    <div
      className="message-navigator"
      data-visible={visible && !empty}
      ref={rail}
      onPointerEnter={(event) => {
        if (event.pointerType !== "touch") setVisible(true);
      }}
      onPointerLeave={() => {
        if (!pinned && !rail.current?.contains(document.activeElement)) close();
      }}
      onFocusCapture={() => setVisible(true)}
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
            setVisible(true);
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
                top: `${((marker.first + marker.last - 1) / 2 / Math.max(1, window?.total ?? 1)) * 100}%`,
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
          window?.nextWindowCursor ||
          windows.length > 0) && (
          <div className="message-navigator-preview" aria-label="Message previews">
            <div className="message-navigator-heading">
              <span>
                {group?.first === group?.last && group ? "Your message" : "Your messages"}
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
            {jumpError && <Notice>{jumpError}</Notice>}
            {(overview.pending || (group && previews.pending)) && (
              <Loading label="Reading messages…" />
            )}
            {items && items.length > 0 && (
              <ol className="message-navigator-items">
                {items.map((item) => (
                  <li key={item.entryId}>
                    <button type="button" onClick={() => jump(item)}>
                      {item.timestamp && (
                        <HistoryTimestamp timestamp={item.timestamp} includeDate />
                      )}
                      <span>{item.text}</span>
                    </button>
                  </li>
                ))}
              </ol>
            )}
            {(previewPages.length > 0 || nextPreview) && group && (
              <div className="message-navigator-paging">
                <button
                  type="button"
                  className="quiet-button"
                  disabled={!previewPages.length}
                  onClick={() => setSelected({ group: group.cursor, pages: [] })}
                >
                  First
                </button>
                <button
                  type="button"
                  className="quiet-button"
                  disabled={!previewPages.length}
                  onClick={() =>
                    setSelected({ group: group.cursor, pages: previewPages.slice(0, -1) })
                  }
                >
                  Previous
                </button>
                <button
                  type="button"
                  className="quiet-button"
                  disabled={!nextPreview}
                  onClick={() =>
                    nextPreview &&
                    setSelected({ group: group.cursor, pages: [...previewPages, nextPreview] })
                  }
                >
                  Next
                </button>
              </div>
            )}
            {(window?.nextWindowCursor || windows.length > 0) && (
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
                {window?.nextWindowCursor && (
                  <button
                    type="button"
                    className="quiet-button"
                    onClick={() =>
                      window.nextWindowCursor && setWindows([...windows, window.nextWindowCursor])
                    }
                  >
                    More groups
                  </button>
                )}
              </div>
            )}
            {window && !window.complete && <p className="meta">Message overview is incomplete.</p>}
          </div>
        )}
    </div>
  );
}
