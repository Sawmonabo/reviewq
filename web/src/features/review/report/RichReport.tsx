import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import type { Root } from "hast";
import { useCallback, useEffect, useLayoutEffect, useMemo, useState } from "react";
import { MarkdownTree } from "../../../components/markdown/MarkdownContent";
import { savedValue, saveValue } from "../../../lib/view-preferences";
import { parentElement, reportBlocks, selectionSource } from "./report-presentation";
import type { ReportBlock } from "./report-presentation";

type Bookmark = { readonly top: number; readonly offset?: number; readonly inset?: number };
type Quote = { readonly text: string; readonly offset: number };
type Selection = { readonly quote?: Quote; readonly fallback: number };

export function RichReport({
  tree,
  source,
  bookmark,
  onQuote,
  onSource,
}: {
  readonly tree: Root;
  readonly source: string;
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
  const blocks = useMemo(() => reportBlocks(tree, source), [tree, source]);
  const usesVirtual = blocks.length > 17;
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
      return [...indexes].sort((a, b) => a - b);
    },
    [pinned, restoreIndex],
  );
  // The measured rows and virtualizer stay inside this renderer.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({
    count: usesVirtual ? blocks.length : 0,
    estimateSize: () => 230,
    getScrollElement: () => scroll,
    getItemKey: (index) => blocks[index]?.offset ?? index,
    scrollMargin: margin,
    overscan: 3,
    rangeExtractor: range,
  });

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

  useLayoutEffect(() => {
    if (!host || !scroll || restored || !saved) return;
    const element = host;
    const container = scroll;
    const position = saved;
    let frame = requestAnimationFrame(restore);
    let attempts = 0;
    function restore() {
      const row = restoreIndex >= 0 ? blockElement(element, restoreIndex) : undefined;
      if (row && position.inset !== undefined) {
        container.scrollTop +=
          row.getBoundingClientRect().top - container.getBoundingClientRect().top - position.inset;
      } else if (restoreIndex >= 0 && usesVirtual) {
        virtual.scrollToIndex(restoreIndex, { align: "start" });
      } else {
        container.scrollTop = position.top;
      }
      if (++attempts < 3) frame = requestAnimationFrame(restore);
      else setRestored(true);
    }
    return () => cancelAnimationFrame(frame);
  }, [host, scroll, restored, saved, restoreIndex, usesVirtual, virtual]);

  useEffect(() => {
    if (!host || !scroll || !restored) return;
    const element = host;
    const container = scroll;
    let last = bookmarkAt(element, container);
    let timer: number | undefined;
    function record() {
      last = bookmarkAt(element, container);
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
  }, [bookmark, host, scroll, restored]);

  useEffect(() => {
    if (!host) return;
    const element = host;
    function capture() {
      const active = getSelection();
      if (!active || active.isCollapsed || !active.rangeCount) {
        if (!document.activeElement?.closest(".report-selection-action")) {
          setSelection(undefined);
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
      const exact = selectionSource(selected, source, start, end);
      setSelection({
        fallback: start.offset,
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
  }, [blocks, host, source, usesVirtual]);

  function jump(index: number) {
    if (!host || !scroll) return;
    if (usesVirtual) virtual.scrollToIndex(index, { align: "start" });
    else {
      const row = blockElement(host, index);
      if (row)
        scroll.scrollTop += row.getBoundingClientRect().top - scroll.getBoundingClientRect().top;
    }
  }

  function renderBlock(block: ReportBlock, index: number) {
    const finding = block.finding;
    return (
      <>
        {finding ? (
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
                {finding.blocking !== undefined && (
                  <span>{finding.blocking ? "Blocking" : "Nonblocking"}</span>
                )}
              </div>
              <MarkdownTree tree={{ type: "root", children: [...finding.header] }} />
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
                      <MarkdownTree tree={{ type: "root", children: [...nodes] }} />
                    </dd>
                  </div>
                ))}
              </dl>
            )}
          </article>
        ) : (
          <div className="report-authored">
            <MarkdownTree tree={{ type: "root", children: [...block.nodes] }} />
          </div>
        )}
        {index === 0 && blocks.slice(1).some((part) => part.name && !part.finding) && (
          <nav className="report-section-nav" aria-label="Report sections">
            {blocks.map((part, position) =>
              position > 0 && part.name && !part.finding ? (
                <button
                  type="button"
                  className="quiet-button"
                  key={part.offset}
                  onClick={() => jump(position)}
                >
                  {part.name}
                </button>
              ) : null,
            )}
          </nav>
        )}
      </>
    );
  }

  return (
    <section className="rich-report" aria-label="Formatted accepted report">
      <div
        className="report-blocks"
        ref={setRoot}
        style={usesVirtual ? { height: virtual.getTotalSize(), position: "relative" } : undefined}
      >
        {usesVirtual
          ? virtual.getVirtualItems().map((item) => {
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
                  {renderBlock(block, item.index)}
                </div>
              );
            })
          : blocks.map((block, index) => (
              <div key={index} data-report-block={index} data-report-offset={block.offset}>
                {renderBlock(block, index)}
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
                ? onQuote(selection.quote.text, [...source.slice(0, selection.quote.offset)].length)
                : onSource([...source.slice(0, selection.fallback)].length)
            }
          >
            Comment on selected finding
          </button>
        </div>
      )}
    </section>
  );
}

function blockElement(root: HTMLElement, index: number) {
  return root.querySelector<HTMLElement>(`[data-report-block="${index}"]`);
}

function blockIndex(node: Node) {
  const author = parentElement(node)?.closest(".report-authored");
  const value = author?.closest<HTMLElement>("[data-report-block]")?.dataset.reportBlock;
  return value === undefined ? -1 : Number(value);
}

function bookmarkAt(host: HTMLElement, scroll: HTMLElement): Bookmark {
  const top = scroll.scrollTop;
  const frame = scroll.getBoundingClientRect();
  for (const row of host.querySelectorAll<HTMLElement>("[data-report-offset]")) {
    const bounds = row.getBoundingClientRect();
    const offset = Number(row.dataset.reportOffset);
    if (bounds.bottom > frame.top && bounds.top < frame.bottom && Number.isSafeInteger(offset)) {
      return { top, offset, inset: bounds.top - frame.top };
    }
  }
  return { top };
}

function readBookmark(key: string): Bookmark | undefined {
  const item = savedValue(key);
  if (
    !item ||
    typeof item !== "object" ||
    !("top" in item) ||
    typeof item.top !== "number" ||
    !Number.isFinite(item.top) ||
    item.top < 0
  )
    return;
  if (
    "offset" in item &&
    typeof item.offset === "number" &&
    Number.isSafeInteger(item.offset) &&
    item.offset >= 0 &&
    "inset" in item &&
    typeof item.inset === "number" &&
    Number.isFinite(item.inset)
  ) {
    return { top: item.top, offset: item.offset, inset: item.inset };
  }
  return { top: item.top };
}
