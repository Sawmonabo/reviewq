import { defaultRangeExtractor, useVirtualizer } from "@tanstack/react-virtual";
import type { Root, RootContent } from "hast";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MarkdownTree } from "../../../components/markdown/MarkdownContent";
import { Notice } from "../../../components/PanelState";

type Block = { readonly offset: number; readonly end: number; readonly children: RootContent[]; readonly name?: string };

export function RichReport({ tree, source, onQuote, onSource }: {
  readonly tree: Root;
  readonly source: string;
  readonly onQuote: (text: string, offset: number) => void;
  readonly onSource: (offset: number) => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  const [scroll, setScroll] = useState<HTMLElement | null>(null);
  const [margin, setMargin] = useState(0);
  const [pinned, setPinned] = useState<readonly number[]>([]);
  const [error, setError] = useState<string>();
  const blocks = useMemo(() => semanticBlocks(tree, source.length), [tree, source.length]);
  const range = useCallback((value: Parameters<typeof defaultRangeExtractor>[0]) => [...new Set([...defaultRangeExtractor(value), ...pinned])].sort((a, b) => a - b), [pinned]);
  const virtual = useVirtualizer({
    count: blocks.length,
    estimateSize: () => 260,
    getScrollElement: () => scroll,
    scrollMargin: margin,
    overscan: 3,
    rangeExtractor: range,
  });
  const usesVirtual = blocks.length > 17;

  useEffect(() => {
    const host = root.current;
    const scroller = host?.closest<HTMLElement>(".drawer-body");
    if (!host || !scroller || !usesVirtual) return;
    setScroll(scroller);
    function measurePosition() {
      if (host && scroller) setMargin(scroller.scrollTop + host.getBoundingClientRect().top - scroller.getBoundingClientRect().top);
    }
    const resize = new ResizeObserver(measurePosition);
    resize.observe(scroller);
    resize.observe(host);
    measurePosition();
    window.addEventListener("resize", measurePosition);
    return () => { resize.disconnect(); window.removeEventListener("resize", measurePosition); };
  }, [usesVirtual]);

  useEffect(() => {
    const host = root.current;
    if (!host || !usesVirtual) return;
    function pin() {
      const selection = getSelection();
      const values: number[] = [];
      if (host && selection && !selection.isCollapsed && selection.rangeCount) {
        const selectionRange = selection.getRangeAt(0);
        for (const node of host.querySelectorAll<HTMLElement>("[data-report-block]")) {
          if (selectionRange.intersectsNode(node)) values.push(Number(node.dataset.reportBlock));
        }
      }
      setPinned(values);
    }
    document.addEventListener("selectionchange", pin);
    return () => document.removeEventListener("selectionchange", pin);
  }, [usesVirtual]);

  function quote() {
    const selection = getSelection();
    if (!selection || selection.isCollapsed || !selection.rangeCount) { setError("Select a passage in the report to quote."); return; }
    const selected = selection.getRangeAt(0);
    const start = blockId(selected.startContainer);
    const end = blockId(selected.endContainer);
    const block = blocks[start];
    if (!block || !root.current?.contains(selected.commonAncestorContainer)) { setError("Select a passage in the report body."); return; }
    const exact = selected.toString();
    const container = root.current.querySelector(`[data-report-block="${start}"]`);
    const preceding = document.createRange();
    setError(undefined);
    if (end === start && container && exact.trim()) {
      preceding.selectNodeContents(container);
      preceding.setEnd(selected.startContainer, selected.startOffset);
      const prefix = preceding.toString();
      const absolute = block.offset + prefix.length;
      if (source.slice(block.offset, absolute + exact.length) === prefix + exact) {
        onQuote(exact, [...source.slice(0, absolute)].length);
        return;
      }
    }
    onSource([...source.slice(0, block.offset)].length);
  }

  return <section className="rich-report stack" aria-label="Formatted accepted report">
    <div className="actions">
      <label>Jump to section <select defaultValue="" onChange={(event) => {
        const index = Number(event.currentTarget.value);
        if (usesVirtual) virtual.scrollToIndex(index, { align: "start" });
        else root.current?.querySelector(`[data-report-block="${index}"]`)?.scrollIntoView({ block: "start" });
      }}><option value="" disabled>Choose heading</option>
        {blocks.map((block, index) => block.name ? <option key={index} value={index}>{block.name}</option> : null)}</select></label>
      <button type="button" onClick={quote}>Quote selection</button>
    </div>
    {error && <Notice>{error}</Notice>}
    <div className="report-blocks" ref={root} style={usesVirtual ? { height: virtual.getTotalSize(), position: "relative" } : undefined}>
      {usesVirtual ? virtual.getVirtualItems().map((item) => {
        const block = blocks[item.index];
        if (!block) return null;
        return <div key={item.key} data-index={item.index} data-report-block={item.index} ref={virtual.measureElement}
          style={{ position: "absolute", top: 0, left: 0, width: "100%", transform: `translateY(${item.start - margin}px)` }}>
          <MarkdownTree tree={{ type: "root", children: block.children }} />
        </div>;
      }) : blocks.map((block, index) => <div key={index} data-report-block={index}><MarkdownTree tree={{ type: "root", children: block.children }} /></div>)}
    </div>
  </section>;
}

function semanticBlocks(tree: Root, total: number): Block[] {
  const blocks: { offset: number; children: RootContent[]; name?: string }[] = [];
  let current: typeof blocks[number] | undefined;
  for (const node of tree.children) {
    const heading = node.type === "element" && /^h[1-6]$/.test(node.tagName) ? node.children.map((content) => content.type === "text" ? content.value : "").join("").trim() : "";
    if (!current || heading || current.children.length >= 22 && node.position?.start.offset != null) {
      current = { offset: node.position?.start.offset ?? 0, children: [], ...(heading ? { name: heading } : {}) };
      blocks.push(current);
    }
    current.children.push(node);
  }
  return blocks.map((block, index) => ({ ...block, end: blocks[index + 1]?.offset ?? total }));
}

function blockId(node: Node) {
  const element = node instanceof Element ? node : node.parentElement;
  const id = element?.closest<HTMLElement>("[data-report-block]")?.dataset.reportBlock;
  return id === undefined ? -1 : Number(id);
}
