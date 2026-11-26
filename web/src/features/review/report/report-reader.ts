import type {
  DisplayBlock,
  DocumentDisplay,
  DrawingHint,
} from "../../../api/generated/contracts/common";
import { reportWindow } from "../../../api/reports";
import type {
  DisplayLayout,
  DisplayResult,
  ReportSummary,
} from "../../../api/generated/contracts/reports";
import { parseMarkdown } from "../../../components/markdown/parse-markdown";
import { parseSettledMarkdown, tableCost } from "../../../components/markdown/settled";
import { groupReportItems, reportBlocks } from "./report-presentation";
import type { GroupPosition, PreparedGroup } from "./report-presentation";
import { isThreadSection, reportText, startsReportBody } from "./report-structure";
import type { AcceptedFindingChange } from "./report-structure";

type Ready = Extract<DisplayResult, { state: "ready" }>;
type Changes = ReadonlyMap<string, AcceptedFindingChange> | null | undefined;
type Group = GroupPosition & {
  readonly first: number;
  readonly last: number;
  readonly previousThreads: boolean;
};
export type ReaderDocument = {
  readonly groups: readonly GroupPosition[];
  readonly prepared: (index: number) => PreparedGroup | undefined;
  readonly byteAt?: (index: number) => number | undefined;
  readonly prepare?: (
    indexes: readonly number[],
    signal: AbortSignal,
    publish: () => void,
    current: (preparing?: number) => boolean,
    pinned: readonly number[],
  ) => Promise<void>;
};

const MAX_GROUP_BYTES = 128 * 1024;
const MAX_RETAINED_BYTES = 320 * 1024;
const RANGE_BYTES = 64 * 1024;
const utf8 = new TextEncoder();
// Report text shows raw HTML as written, such as `Vec<u8>`, with `<br>` as a line break.
export const reportMarkdown = { literalHtml: true } as const;

export function canWindowReport(report: ReportSummary) {
  return (
    report.byteLength >= 64 * 1024 ||
    (Boolean(report.changeCount) && report.byteLength >= 24 * 1024)
  );
}

export function wholeDocument(
  source: string,
  tree: NonNullable<ReturnType<typeof parseMarkdown>>,
  changes: Changes,
  display: DocumentDisplay | null | undefined,
): ReaderDocument {
  const groups = reportBlocks(tree, source, changes);
  const slice = { text: source, utf16: 0, scalar: 0 };
  return {
    groups,
    prepared: (index) => {
      const block = groups[index];
      return block ? { block, source: slice, drawings: display?.drawings } : undefined;
    },
  };
}

export function indexedDocument(
  runId: string,
  report: ReportSummary,
  opened: Ready,
  changes: Changes,
  base: string | undefined,
  savedOffset: number | undefined,
): ReaderDocument {
  const layout = opened.layout;
  if (
    !layout ||
    layout.blocks.length !== opened.totalBlocks ||
    layout.byteLength !== report.byteLength ||
    layout.scalarLength !== report.totalCodePoints
  )
    throw new Error("The report layout is incomplete.");
  const spans = layout.blocks;
  for (const [index, span] of spans.entries()) {
    const next = spans[index + 1];
    if (
      span.byteStart >= span.byteEnd ||
      span.byteEnd !== (next?.byteStart ?? layout.byteLength) ||
      span.utf16Start >= (next?.utf16Start ?? layout.utf16Length) ||
      span.scalarStart >= (next?.scalarStart ?? layout.scalarLength)
    )
      throw new Error("The report layout contains an invalid range.");
  }
  const groups = planGroups(layout, base);
  if (groups.length <= 17) throw new Error("This report fits its full formatted view.");
  const owners = new Array<number>(spans.length);
  for (const [index, group] of groups.entries())
    for (let i = group.first; i <= group.last; i++) owners[i] = index;
  const byStart = new Map(spans.map((span, index) => [span.byteStart, index]));
  const raw = new Map<number, DisplayBlock>();
  const prepared = new Map<number, PreparedGroup>();
  const retainedCost = new Map<number, number>();
  let previousPins = new Set<number>();

  function receive(blocks: readonly DisplayBlock[], destination: number) {
    for (const block of blocks) {
      const index = byStart.get(block.byteStart) ?? -1;
      const span = spans[index];
      const next = spans[index + 1];
      if (
        !span ||
        block.byteEnd !== span.byteEnd ||
        block.kind !== span.kind ||
        block.utf16Start !== span.utf16Start ||
        block.scalarStart !== span.scalarStart ||
        block.markdown.length !== (next?.utf16Start ?? layout!.utf16Length) - span.utf16Start ||
        utf8.encode(block.markdown).length !== span.byteEnd - span.byteStart
      )
        throw new Error("The displayed report span changed.");
      if (spans[index]?.kind === "table" && owners[index] !== destination) continue;
      raw.set(index, block);
    }
  }

  function compile(index: number, available: number) {
    if (prepared.has(index)) return true;
    const group = groups[index]!;
    const parts: DisplayBlock[] = [];
    const drawings = new Map<string, DrawingHint>();
    for (let i = group.first; i <= group.last; i++) {
      const part = raw.get(i);
      if (!part) return false;
      parts.push(part);
      for (const drawing of part.display?.drawings ?? [])
        drawings.set(drawing.sourceSha256, drawing);
    }
    const text = parts.map((part) => part.markdown).join("");
    const sourceBytes = spans[group.last]!.byteEnd - spans[group.first]!.byteStart;
    const extra = parts.reduce((total, part) => total + tableCost(part.display?.table), 0);
    const provision = extra <= available;
    const tree = provision
      ? parseSettledMarkdown(
          text,
          parts.map((part) => ({ markdown: part.markdown, table: part.display?.table })),
          base,
          reportMarkdown,
        )
      : parseMarkdown(text, base, reportMarkdown);
    if (!tree) throw new Error("The report group exceeded its formatting limits.");
    const blocks = reportBlocks(
      tree,
      text,
      changes,
      index === 0 ? undefined : { previousThreads: group.previousThreads },
    );
    const block = blocks[0];
    if (
      !block ||
      blocks.length !== 1 ||
      block.name !== group.name ||
      block.finding?.id !== group.findingId
    )
      throw new Error("This report requires full-document formatting.");
    const first = spans[group.first]!;
    prepared.set(index, {
      block,
      source: { text, utf16: first.utf16Start, scalar: first.scalarStart },
      drawings: drawings.size ? [...drawings.values()] : undefined,
    });
    retainedCost.set(index, sourceBytes + (provision ? extra : 0));
    for (let i = group.first; i <= group.last; i++) raw.delete(i);
    return true;
  }

  const opening = Math.max(
    0,
    groups.findIndex((group) => group.offset === savedOffset),
  );
  receive(opened.blocks, opening);
  const openingGroup = groups[opening]!;
  compile(
    opening,
    MAX_RETAINED_BYTES - (spans[openingGroup.last]!.byteEnd - spans[openingGroup.first]!.byteStart),
  );

  async function prepare(
    indexes: readonly number[],
    signal: AbortSignal,
    publish: () => void,
    current: (preparing?: number) => boolean,
    pinned: readonly number[],
  ) {
    // Native selection and keyboard focus retain only trees that are already on
    // screen. A long selection may retain more of this single 1 MiB rich report.
    const held = new Set(pinned.filter((index) => prepared.has(index)));
    for (const index of previousPins)
      if (!held.has(index) && !indexes.includes(index)) {
        prepared.delete(index);
        retainedCost.delete(index);
      }
    previousPins = held;
    const admitted = new Set(held);
    const heldBytes = [...held].reduce((sum, index) => sum + (retainedCost.get(index) ?? 0), 0);
    let bytes = 0;
    for (const index of [...indexes, ...Array.from(prepared.keys()).reverse()]) {
      if (admitted.has(index)) continue;
      const group = groups[index]!;
      const size =
        retainedCost.get(index) ?? spans[group.last]!.byteEnd - spans[group.first]!.byteStart;
      if (
        bytes + size > MAX_RETAINED_BYTES ||
        (admitted.size - held.size >= 12 && !indexes.includes(index))
      )
        continue;
      bytes += size;
      admitted.add(index);
    }
    for (const index of prepared.keys()) {
      if (!admitted.has(index)) {
        prepared.delete(index);
        retainedCost.delete(index);
      }
    }
    for (const index of indexes) {
      const value = prepared.get(index);
      if (value) {
        prepared.delete(index);
        prepared.set(index, value);
      }
    }
    // Retain neighboring source from a previous batch, within the same byte ledger.
    let adjacentBytes = 0;
    for (const [index] of raw) {
      if (admitted.has(owners[index]!)) continue;
      const size =
        spans[index]!.byteEnd - spans[index]!.byteStart + tableCost(raw.get(index)?.display?.table);
      if (bytes + size > MAX_RETAINED_BYTES || adjacentBytes + size > RANGE_BYTES)
        raw.delete(index);
      else {
        bytes += size;
        adjacentBytes += size;
      }
    }
    for (const index of indexes) {
      if (!admitted.has(index)) continue;
      if (prepared.has(index)) continue;
      if (!current(index)) return;
      const group = groups[index]!;
      for (let first = group.first; first <= group.last; first++) {
        if (raw.has(first)) continue;
        let last = first;
        while (last + 1 < spans.length && last - first + 1 < 32) {
          const next = spans[last + 1]!;
          if (
            raw.has(last + 1) ||
            prepared.has(owners[last + 1]!) ||
            (next.kind === "table" && owners[last + 1] !== index) ||
            next.byteEnd - spans[first]!.byteStart > RANGE_BYTES
          )
            break;
          if (!admitted.has(owners[last + 1]!)) {
            const size = next.byteEnd - next.byteStart;
            if (bytes + size > MAX_RETAINED_BYTES || adjacentBytes + size > RANGE_BYTES) break;
            bytes += size;
            adjacentBytes += size;
          }
          last++;
        }
        const { data } = await reportWindow(runId, report, signal, {
          byteOffset: spans[first]!.byteStart,
          byteEnd: spans[last]!.byteEnd,
          tableByteEnd: spans[Math.min(last, group.last)]!.byteEnd,
        });
        signal.throwIfAborted();
        const result = data.result;
        if (
          result.state !== "ready" ||
          result.totalBlocks !== spans.length ||
          result.blocks.length !== last - first + 1 ||
          result.blocks[0].byteStart !== spans[first]!.byteStart ||
          result.blocks.at(-1)?.byteEnd !== spans[last]!.byteEnd
        )
          throw new Error("The requested report group is unavailable.");
        receive(result.blocks, index);
        if (!current()) return;
      }
      signal.throwIfAborted();
      if (!compile(index, Math.max(0, MAX_RETAINED_BYTES - bytes - heldBytes)))
        throw new Error("The report group is incomplete.");
      bytes +=
        retainedCost.get(index)! - (spans[group.last]!.byteEnd - spans[group.first]!.byteStart);
      publish();
      // Yield between independent groups so a jump can supersede look-ahead work.
      if (indexes.length > 1) await new Promise<void>((resolve) => window.setTimeout(resolve, 0));
      if (!current()) return;
      signal.throwIfAborted();
    }
  }

  return {
    groups,
    prepared: (index) => prepared.get(index),
    byteAt: (index) => spans[groups[index]!.first]!.byteStart,
    prepare,
  };
}

function planGroups(layout: DisplayLayout, base: string | undefined) {
  const spans = layout.blocks;
  const headings = spans.filter((s) => s.kind === "heading" && (s.headingLevel ?? 7) <= 3);
  if (headings.some((h) => !h.headingMarkdown))
    throw new Error("The report outline is incomplete.");
  const tree = parseMarkdown(
    headings.map((h) => h.headingMarkdown).join("\n\n"),
    base,
    reportMarkdown,
  );
  const nodes = tree?.children.filter((n) => n.type === "element");
  if (!nodes || nodes.length !== headings.length)
    throw new Error("The report headings require full-document formatting.");
  const labels = new Map<number, { level: number; label: string; section: boolean }>();
  for (const [index, span] of headings.entries()) {
    const node = nodes[index]!;
    if (node.type !== "element" || node.tagName !== `h${span.headingLevel}`)
      throw new Error("The report heading level changed.");
    labels.set(span.byteStart, {
      level: span.headingLevel!,
      label: reportText(node).trim(),
      section: startsReportBody(node),
    });
  }
  const bodyStart = spans.findIndex(
    (span) => span.kind === "table" || labels.get(span.byteStart)?.section,
  );
  // Cover transformations hide the cover lines and join the verdict to its rationale.
  // Keep the complete cover inside its first presentation group.
  if (bodyStart < 1 || bodyStart > 18 || spans[bodyStart]?.kind === "table")
    throw new Error("The report cover needs its full document.");
  const pieces = groupReportItems(spans, (span) => {
    const heading =
      span.byteStart >= spans[bodyStart]!.byteStart ? labels.get(span.byteStart) : undefined;
    return {
      offset: span.utf16Start,
      ...(heading && { level: heading.level, label: heading.label }),
    };
  });
  let first = 0;
  let previousThreads = false;
  return pieces.map((group): Group => {
    const last = first + group.items.length - 1;
    if (spans[last]!.byteEnd - spans[first]!.byteStart > MAX_GROUP_BYTES)
      throw new Error("The report section needs full-document formatting.");
    const entry = {
      offset: group.offset,
      first,
      last,
      previousThreads,
      ...(group.name && { name: group.name }),
      ...(group.findingId && { findingId: group.findingId }),
    };
    if (group.name) previousThreads = isThreadSection(group.name);
    first = last + 1;
    return entry;
  });
}
