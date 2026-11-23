import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import { describeError } from "../../../lib/presentation";

export function useHistorySelection(viewport: RefObject<HTMLDivElement | null>) {
  const [pinned, setPinned] = useState<ReadonlySet<string>>(() => new Set());
  const [selectedText, setSelectedText] = useState(false);
  const [diagramFocus, setDiagramFocus] = useState<string>();
  const focusedDiagram = !selectedText && pinned.size === 1 ? diagramFocus : undefined;
  const selectionState = useRef<{ hasText: boolean; ids: Set<string>; diagram?: string }>({
    hasText: false,
    ids: new Set(),
  });

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
          for (const node of host.querySelectorAll<HTMLElement>(
            "[data-native-entry], [data-delivery-row]",
          )) {
            const id =
              node.dataset.deliveryRow ?? node.dataset.questionRow ?? node.dataset.nativeEntry;
            if (id && range.intersectsNode(node)) ids.add(id);
          }
      }
      const active = document.activeElement;
      const focused = active?.closest<HTMLElement>("[data-native-entry], [data-delivery-row]");
      const id =
        focused && host.contains(focused)
          ? (focused.dataset.deliveryRow ??
            focused.dataset.questionRow ??
            focused.dataset.nativeEntry)
          : undefined;
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
  }, [viewport]);

  function clear() {
    if (selectedText) getSelection()?.removeAllRanges();
    setSelectedText(false);
    setDiagramFocus(undefined);
    setPinned(new Set());
    selectionState.current = { hasText: false, ids: new Set() };
  }

  function isDiagramVisible(entryId: string) {
    const host = viewport.current;
    const figure = document.activeElement?.closest(".mermaid-figure");
    if (
      !host ||
      !figure ||
      !host.contains(figure) ||
      figure.closest<HTMLElement>("[data-native-entry]")?.dataset.nativeEntry !== entryId
    )
      return false;
    const bounds = figure.getBoundingClientRect();
    const frame = host.getBoundingClientRect();
    return bounds.height > 0 && bounds.bottom > frame.top && bounds.top < frame.bottom;
  }

  return { pinned, selectedText, focusedDiagram, isDiagramVisible, clear };
}

export function HistorySelection({
  onClear,
  onError,
}: {
  readonly onClear: () => void;
  readonly onError: (message: string) => void;
}) {
  return (
    <div className="timeline-controls">
      <button
        type="button"
        onClick={() => {
          const value = getSelection()?.toString();
          if (value)
            void navigator.clipboard
              .writeText(value)
              .catch((failure: unknown) => onError(describeError(failure)));
        }}
      >
        Copy selection
      </button>
      <button type="button" onClick={onClear}>
        Clear selection
      </button>
    </div>
  );
}
