import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { RootContent } from "hast";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { MarkdownTree } from "../../../components/markdown/MarkdownContent";
import { bookmarkAt, readBookmark, restoreBookmark } from "./report-position";
import { saveValue } from "../../../lib/view-preferences";
import {
  parentElement,
  scalarOffset,
  reportParagraphQuote,
  selectionSource,
} from "./report-presentation";
import type { PreparedGroup, ReportBlock } from "./report-presentation";
import { isRetired, outcomeLabels } from "./report-structure";
import type { ReaderDocument } from "./report-reader";
import { useReportPreparation } from "./useReportPreparation";

type Quote = { readonly text: string; readonly offset: number };
type Selection = {
  readonly quote?: Quote;
  readonly fallback: number;
  readonly paragraph?: true;
};

export function RichReport({
  document: reader,
  onFallback,
  report,
  bookmark,
  onQuote,
  onSource,
}: {
  readonly document: ReaderDocument;
  readonly onFallback: () => void;
  readonly report: ReportSummary;
  readonly bookmark: string;
  readonly onQuote: (text: string, offset: number) => void;
  readonly onSource: (offset: number) => void;
}) {
  const [host, setHost] = useState<HTMLDivElement | null>(null);
  const [scroll, setScroll] = useState<HTMLElement | null>(null);
  const [margin, setMargin] = useState(0);
  const [pinned, setPinned] = useState<readonly number[]>([]);
  const [selection, setSelection] = useState<Selection>();
  const [saved] = useState(() => readBookmark(bookmark));
  const [restored, setRestored] = useState(!saved);
  const blocks = reader.groups;

  const [focused, setFocused] = useState<number>();
  const [navigation, setNavigation] = useState<{
    index: number;
    phase: "hover" | "pressed" | "jump";
  }>();
  const hoverTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(hoverTimer.current), []);
  const destination = navigation?.index;
  // A section title may occupy its entire group, followed immediately by a card or table.
  const nextGroup =
    destination !== undefined && destination + 1 < blocks.length ? destination + 1 : undefined;
  const stage = navigation?.phase === "jump";
  const destinationReady =
    destination !== undefined &&
    Boolean(reader.prepared(destination)) &&
    (nextGroup === undefined || Boolean(reader.prepared(nextGroup)));
  const openDetails = useRef(new Set<number>());

  const navigationAfter = Math.max(
    0,
    blocks.findIndex((block) => block.name || block.findingId) - 1,
  );
  const sections = blocks.flatMap((block, index) =>
    block.name && !block.findingId ? [{ index, label: block.name }] : [],
  );
  const usesVirtual = Boolean(reader.prepare) || blocks.length > 17;
  const restoreIndex =
    !restored && saved?.offset !== undefined
      ? blocks.findIndex((block) => block.offset === saved.offset)
      : -1;
  const setRoot = useCallback((node: HTMLDivElement | null) => {
    setHost(node);
    setScroll(node?.closest<HTMLElement>(".drawer-body") ?? null);
  }, []);
  const range = useCallback(
    (value: Parameters<typeof defaultRangeExtractor>[0]) => {
      const indexes = new Set([...defaultRangeExtractor(value), ...pinned]);
      if (restoreIndex >= 0) indexes.add(restoreIndex);
      if (focused !== undefined) indexes.add(focused);
      if (stage && destination !== undefined) {
        indexes.add(destination);
        if (nextGroup !== undefined) indexes.add(nextGroup);
      }
      return [...indexes].sort((a, b) => a - b);
    },
    [pinned, restoreIndex, focused, stage, destination, nextGroup],
  );
  // The measured rows and virtualizer stay inside this renderer.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({
    count: usesVirtual ? blocks.length : 0,
    estimateSize: () => 230,
    initialRect: { width: window.innerWidth, height: window.innerHeight },
    getScrollElement: () => scroll,
    getItemKey: (index) => `${index}:${blocks[index]?.offset ?? -1}`,
    scrollMargin: margin,
    overscan: 3,
    rangeExtractor: range,
  });

  const items = virtual.getVirtualItems();
  const destinationSize = stage
    ? items.find((item) => item.index === destination)?.size
    : undefined;
  const nextSize = stage ? items.find((item) => item.index === nextGroup)?.size : undefined;
  const firstVisible = virtual.range?.startIndex ?? 0;
  const lastVisible = virtual.range?.endIndex ?? 0;
  const seeking = restoreIndex >= 0 && (restoreIndex < firstVisible || restoreIndex > lastVisible);
  const targetIndexes =
    destination === undefined
      ? []
      : nextGroup === undefined
        ? [destination]
        : [destination, nextGroup];
  const visible = !seeking
    ? items
        .filter((item) => item.index >= firstVisible && item.index <= lastVisible)
        .map((item) => item.index)
    : [];
  const nearby = !seeking
    ? items
        .filter((item) => item.index >= firstVisible - 3 && item.index <= lastVisible + 3)
        .map((item) => item.index)
    : [];
  const urgent = navigation?.phase === "jump" || navigation?.phase === "pressed";
  const wanted = !reader.prepare
    ? ""
    : [
        ...new Set([
          ...(restoreIndex >= 0 ? [restoreIndex] : []),
          ...(urgent ? targetIndexes : []),
          ...visible,
          ...(!urgent ? targetIndexes : []),
          ...nearby,
        ]),
      ].join(",");
  useReportPreparation(
    reader,
    wanted,
    [...pinned, focused].join(","),
    urgent && (!stage || !destinationReady),
    host,
    onFallback,
  );

  useLayoutEffect(() => {
    if (!host || !scroll || !usesVirtual) return;
    const element = host;
    const container = scroll;
    function measure() {
      setMargin(
        container.scrollTop +
          element.getBoundingClientRect().top -
          container.getBoundingClientRect().top,
      );
    }
    const observer = new ResizeObserver(measure);
    observer.observe(container);
    observer.observe(element);
    const frame = requestAnimationFrame(measure);
    return () => {
      cancelAnimationFrame(frame);
      observer.disconnect();
    };
  }, [host, scroll, usesVirtual]);

  const restoreReady = restoreIndex < 0 || Boolean(reader.prepared(restoreIndex));
  useLayoutEffect(() => {
    if (!host || !scroll || restored || !saved || !restoreReady) return;
    return restoreBookmark(host, scroll, saved, restoreIndex, () => setRestored(true));
  }, [host, scroll, restored, saved, restoreIndex, restoreReady]);

  useEffect(() => {
    if (!host || !scroll || !restored) return;
    const element = host;
    const container = scroll;
    let last = bookmarkAt(element, container, reader);
    let timer: number | undefined;
    function record() {
      last = bookmarkAt(element, container, reader);
      if (timer !== undefined) window.clearTimeout(timer);
      timer = window.setTimeout(commit, 180);
    }
    function commit() {
      saveValue(bookmark, last);
    }
    container.addEventListener("scroll", record, { passive: true });
    window.addEventListener("pagehide", commit);
    return () => {
      container.removeEventListener("scroll", record);
      window.removeEventListener("pagehide", commit);
      if (timer !== undefined) window.clearTimeout(timer);
      commit();
    };
  }, [bookmark, host, scroll, restored, reader]);

  useEffect(() => {
    if (!host) return;
    const element = host;
    function capture() {
      const active = getSelection();
      if (!active || active.isCollapsed || !active.rangeCount) {
        if (!document.activeElement?.closest(".report-selection-action")) {
          setSelection((current) => (current?.paragraph ? current : undefined));
          setPinned([]);
        }
        return;
      }
      const selected = active.getRangeAt(0);
      const first = blockIndex(selected.startContainer);
      const last = blockIndex(selected.endContainer);
      const start = blocks[first];
      const end = blocks[last];
      if (!start || !end || !element.contains(selected.commonAncestorContainer)) {
        setSelection(undefined);
        setPinned([]);
        return;
      }
      const inSourceOrder = blocks
        .slice(first, last + 1)
        .every((block, index) => block.sourceOrder === start.sourceOrder + index);
      const groups = blocks.slice(first, last + 1).map((_, i) => reader.prepared(first + i));
      const firstGroup = groups[0];
      if (!firstGroup) return;
      const exact =
        inSourceOrder && groups.every((group): group is PreparedGroup => Boolean(group))
          ? selectionSource(selected, groups)
          : undefined;
      setSelection({
        fallback: scalarOffset(firstGroup.source, firstGroup.block.offset),
        ...(exact ? { quote: exact } : {}),
      });
      if (usesVirtual) {
        const indexes = [...element.querySelectorAll<HTMLElement>("[data-report-block]")]
          .filter((row) => selected.intersectsNode(row))
          .map((row) => Number(row.dataset.reportBlock));
        setPinned(indexes);
      }
    }
    document.addEventListener("selectionchange", capture);
    return () => document.removeEventListener("selectionchange", capture);
  }, [blocks, host, reader, usesVirtual]);

  useEffect(() => {
    if (!stage || destinationReady || !scroll) return;
    const events = new AbortController();
    const cancel = () => {
      window.clearTimeout(hoverTimer.current);
      setNavigation(undefined);
    };
    for (const name of ["wheel", "touchmove", "pointerdown", "keydown"])
      scroll.addEventListener(name, cancel, { signal: events.signal, passive: true });
    return () => events.abort();
  }, [stage, destinationReady, scroll]);

  useLayoutEffect(() => {
    if (!stage || destination === undefined || !destinationReady || !host || !scroll) return;
    const row = blockElement(host, destination);
    if (!row) return;
    // The staged rows can still have the measured size of their loading boxes.
    // Commit their actual sizes so later rows cannot overlap the destination.
    if (usesVirtual) {
      const next = nextGroup === undefined ? undefined : blockElement(host, nextGroup);
      if (nextGroup !== undefined && !next) return;
      const height = Math.round(row.getBoundingClientRect().height);
      const nextHeight = next ? Math.round(next.getBoundingClientRect().height) : undefined;
      if (destinationSize !== height || nextSize !== nextHeight) {
        virtual.resizeItem(destination, height);
        if (nextGroup !== undefined && nextHeight !== undefined)
          virtual.resizeItem(nextGroup, nextHeight);
        return;
      }
    }
    scroll.scrollTop += row.getBoundingClientRect().top - scroll.getBoundingClientRect().top;
    return restoreBookmark(host, scroll, { top: scroll.scrollTop, inset: 0 }, destination, () =>
      setNavigation(undefined),
    );
  }, [
    stage,
    destination,
    destinationReady,
    nextGroup,
    destinationSize,
    nextSize,
    usesVirtual,
    virtual,
    host,
    scroll,
  ]);

  function hint(index: number, priority: "hover" | "pressed", immediate: boolean) {
    if (!reader.prepare) return;
    window.clearTimeout(hoverTimer.current);
    function start() {
      setNavigation((current) =>
        current?.phase === "jump" || (current?.index === index && current.phase === "pressed")
          ? current
          : { index, phase: priority },
      );
    }
    if (immediate) start();
    else hoverTimer.current = window.setTimeout(start, 65);
  }

  function leave(index: number) {
    window.clearTimeout(hoverTimer.current);
    setNavigation((current) =>
      current?.index === index && current.phase !== "jump" ? undefined : current,
    );
  }

  function jump(index: number) {
    if (!host || !scroll) return;
    window.clearTimeout(hoverTimer.current);
    setRestored(true);
    if (reader.prepare) {
      setNavigation({ index, phase: "jump" });
      return;
    }
    if (usesVirtual) virtual.scrollToIndex(index, { align: "start" });
    else {
      const row = blockElement(host, index);
      if (row)
        scroll.scrollTop += row.getBoundingClientRect().top - scroll.getBoundingClientRect().top;
    }
  }

  function renderBlock(block: ReportBlock, index: number) {
    const finding = block.finding;
    const change = finding?.change;
    const outcome = change ? outcomeLabels[change.conclusion] : undefined;
    const retired = isRetired(change);
    const card = finding && (
      <article
        className="report-authored report-finding"
        data-blocking={finding.blocking}
        aria-label={finding.id}
      >
        <header className="report-finding-header">
          <div className="report-finding-kicker">
            <strong>{finding.id}</strong>
            {finding.severity && (
              <span className="report-finding-severity">{finding.severity}</span>
            )}
            {finding.category && <span>{finding.category}</span>}
            {finding.blocking !== undefined && (
              <span>{finding.blocking ? "Blocking" : "Nonblocking"}</span>
            )}
            {outcome && (
              <span className="report-finding-assessment" title={change?.reason}>
                {!retired && change?.checkedSha === report.reviewedSha ? "Updated · " : ""}
                {outcome}
              </span>
            )}
          </div>
          <AuthoredContent nodes={finding.header} />
        </header>
        {finding.fields.length > 0 && (
          <dl className="report-finding-fields">
            {finding.fields.map(({ label, nodes }, ordinal) => (
              <div
                className="report-finding-field"
                key={`${ordinal}:${label}`}
                data-field={label.toLowerCase()}
              >
                <dt>{label}</dt>
                <dd>
                  <AuthoredContent nodes={nodes} />
                </dd>
              </div>
            ))}
          </dl>
        )}
      </article>
    );
    return (
      <>
        {finding ? (
          retired ? (
            <details
              className="report-retired-finding"
              open={openDetails.current.has(index)}
              onToggle={(event) => {
                if (event.currentTarget.open) openDetails.current.add(index);
                else openDetails.current.delete(index);
              }}
            >
              <summary>
                <strong>
                  {finding.id} · {outcome}
                </strong>
                {finding.title && ` · ${finding.title}`}
              </summary>
              {change?.reason && <p className="meta">{change.reason}</p>}
              {card}
            </details>
          ) : (
            card
          )
        ) : (
          <div className="report-authored">
            <AuthoredContent nodes={block.nodes} />
          </div>
        )}
        {index === navigationAfter && sections.length > 0 && (
          <nav className="report-section-nav" aria-label="Report sections">
            {sections.map(({ index: target, label }) => (
              <button
                type="button"
                className="quiet-button"
                key={target}
                onPointerMove={(event) => {
                  if (event.pointerType !== "touch" && (event.movementX || event.movementY))
                    hint(target, "hover", false);
                }}
                onPointerLeave={(event) => {
                  if (event.pointerType !== "touch") leave(target);
                }}
                onPointerCancel={() => leave(target)}
                onFocus={() => hint(target, "hover", true)}
                onBlur={() => leave(target)}
                onPointerDown={(event) => {
                  if (event.button === 0 && event.isPrimary) hint(target, "pressed", true);
                }}
                aria-busy={(stage && target === destination && !destinationReady) || undefined}
                onClick={() => jump(target)}
              >
                {label}
              </button>
            ))}
          </nav>
        )}
      </>
    );
  }

  function renderPrepared(index: number) {
    const group = reader.prepared(index);
    return group ? (
      renderBlock(group.block, index)
    ) : (
      <div
        className="report-preparing"
        aria-busy="true"
        style={{ minHeight: virtual.measurementsCache[index]?.size ?? 230 }}
      />
    );
  }

  return (
    <section className="rich-report" aria-label="Formatted accepted report">
      <div
        className="report-blocks"
        ref={setRoot}
        onFocusCapture={(event) => {
          const row = event.target.closest<HTMLElement>("[data-report-block]");
          setFocused(row ? Number(row.dataset.reportBlock) : undefined);
        }}
        onBlurCapture={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) setFocused(undefined);
        }}
        onPointerDown={() => {
          setSelection((current) => (current?.paragraph ? undefined : current));
        }}
        onClick={(event) => {
          const target = event.target;
          if (
            !(target instanceof HTMLElement) ||
            target.closest("a, button, input, textarea, select, summary, [contenteditable]") ||
            getSelection()?.isCollapsed === false
          )
            return;
          const paragraph = target.closest<HTMLElement>("p[data-report-paragraph-start]");
          const group = paragraph ? reader.prepared(blockIndex(paragraph)) : undefined;
          const block = group?.block;
          if (!paragraph || !block || !group) return;
          const quote = reportParagraphQuote(paragraph, group.source.text, block);
          if (!quote) return;
          const offset = scalarOffset(group.source, quote.offset);
          setSelection({ quote: { text: quote.text, offset }, fallback: offset, paragraph: true });
          if (usesVirtual) setPinned([blockIndex(paragraph)]);
        }}
        style={usesVirtual ? { height: virtual.getTotalSize(), position: "relative" } : undefined}
      >
        {usesVirtual
          ? items.map((item) => {
              const block = blocks[item.index];
              if (!block) return null;
              return (
                <div
                  key={item.key}
                  data-index={item.index}
                  data-report-block={item.index}
                  data-report-offset={block.offset}
                  ref={virtual.measureElement}
                  style={{
                    position: "absolute",
                    inset: "0 0 auto",
                    transform: `translateY(${item.start - margin}px)`,
                  }}
                >
                  {renderPrepared(item.index)}
                </div>
              );
            })
          : blocks.map((block, index) => (
              <div key={index} data-report-block={index} data-report-offset={block.offset}>
                {renderPrepared(index)}
              </div>
            ))}
      </div>
      {selection && (
        <div className="report-selection-action">
          <button
            className="secondary"
            type="button"
            onPointerDown={(event) => event.preventDefault()}
            onClick={() =>
              selection.quote
                ? onQuote(selection.quote.text, selection.quote.offset)
                : onSource(selection.fallback)
            }
          >
            Comment on selected finding
          </button>
        </div>
      )}
    </section>
  );
}

const AuthoredContent = memo(function AuthoredContent({
  nodes,
}: {
  readonly nodes: readonly RootContent[];
}) {
  return <MarkdownTree tree={{ type: "root", children: [...nodes] }} />;
});

function blockElement(root: HTMLElement, index: number) {
  return root.querySelector<HTMLElement>(`[data-report-block="${index}"]`);
}

function blockIndex(node: Node) {
  const author = parentElement(node)?.closest(".report-authored");
  const value = author?.closest<HTMLElement>("[data-report-block]")?.dataset.reportBlock;
  return value === undefined ? -1 : Number(value);
}
