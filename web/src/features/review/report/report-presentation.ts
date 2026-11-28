import type { DrawingHint } from "../../../api/generated/contracts/common";
import type { Element, Root, RootContent } from "hast";
import { breakLongNames, reportStructure, reportText as text } from "./report-structure";
import type { AcceptedFindingChange } from "./report-structure";

type Field = { readonly label: string; readonly nodes: readonly RootContent[] };
type AuthoredFinding = {
  readonly id: string;
  readonly severity: string;
  readonly category: string;
  readonly blocking: boolean;
  readonly change?: AcceptedFindingChange;
  readonly header: readonly RootContent[];
  readonly fields: readonly Field[];
};
export type ReportBlock = GroupPosition & {
  readonly end: number;
  readonly nodes: readonly RootContent[];
  readonly name?: string;
  readonly finding?: AuthoredFinding;
};

// The composed card: `### F-01 · {title}`, then `**P1 · {category} · Blocking**`.
const findingHeading = /^(F-\d{2,}) · /;
const cardLine = /^(P[0-3]) · ([^·\n]+) · (Blocking|Nonblocking)$/;
// The composed field labels in the order ReviewQ writes them, each a bold `Label:` that opens
// its paragraph. The last field opens with a bold paragraph that says where the comment goes,
// such as `**Proposed comment — new comment at invoices.rs:74:**`, and keeps that paragraph as
// its first line. A label out of this order, such as one quoted inside a card's details, stays
// part of the field before it.
const fieldOrder = new Map([
  ["Location", 0],
  ["Issue", 1],
  ["Why it matters", 2],
  ["Confirmed", 3],
  ["Evidence", 3],
]);
const proposedComment = { label: "Proposed comment", order: 4, lead: /^Proposed comment — .+:$/ };

export type GroupPosition = {
  readonly offset: number;
  readonly name?: string;
  readonly findingId?: string;
};

export function groupReportItems<T>(
  items: readonly T[],
  describe: (item: T) => {
    readonly level?: number;
    readonly label?: string;
    readonly offset?: number;
  },
) {
  const groups: { offset: number; items: T[]; name?: string; findingId?: string }[] = [];
  let current = groups.at(-1);
  let hasSection = false;
  for (const item of items) {
    const { level, label, offset } = describe(item);
    const heading =
      level !== undefined && level >= (hasSection ? 1 : 2) && level <= 3 ? label : undefined;
    if (
      !current ||
      heading ||
      (!current.findingId && current.items.length >= 24 && offset !== undefined)
    ) {
      const id = heading && level === 3 ? findingHeading.exec(heading)?.[1] : undefined;
      current = {
        offset: offset ?? -1,
        items: [],
        ...(heading && level === 2 && { name: heading }),
        ...(id && { findingId: id }),
      };
      groups.push(current);
    }
    if (heading) hasSection = true;
    if (current.offset < 0 && offset !== undefined) current.offset = offset;
    current.items.push(item);
  }
  return groups;
}

export function reportBlocks(
  tree: Root,
  source: string,
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
  body?: { readonly previousThreads: boolean },
): readonly ReportBlock[] {
  const groups = groupReportItems(reportStructure(tree.children, source, body), (node) => {
    const level =
      node.type === "element" && /^h[1-3]$/.test(node.tagName)
        ? Number(node.tagName[1])
        : undefined;
    return {
      ...(level && { level, label: text(node).trim() }),
      ...(node.position?.start.offset !== undefined && { offset: node.position.start.offset }),
    };
  });
  return groups.map((group, index) => {
    const finding = authoredFinding(group.items, source, changes);
    return {
      offset: group.offset,
      end: groups[index + 1]?.offset ?? source.length,
      nodes: finding ? [] : authored(group.items),
      ...(group.name && { name: group.name }),
      ...(finding && { finding, findingId: finding.id }),
    };
  });
}

export type ReportText = { readonly text: string; readonly utf16: number; readonly scalar: number };
export type PreparedGroup = {
  readonly block: ReportBlock;
  readonly source: ReportText;
  readonly drawings?: readonly DrawingHint[] | undefined;
};

export function selectionSource(range: Range, groups: readonly PreparedGroup[]) {
  const start = groups[0],
    end = groups.at(-1);
  if (!start || !end || !range.toString().trim() || !followsSourceRows(range)) return;
  const from = sourcePoint(start.source.text, range, "start");
  const to = sourcePoint(end.source.text, range, "end");
  if (from === undefined || to === undefined || from < start.block.offset || to > end.block.end)
    return;
  const slices = [...new Set(groups.map((group) => group.source))];
  if (
    !slices.every(
      (slice, i) => i === 0 || slices[i - 1]!.utf16 + slices[i - 1]!.text.length === slice.utf16,
    )
  )
    return;
  const exact =
    slices.length === 1
      ? start.source.text.slice(from, to)
      : [
          start.source.text.slice(from),
          ...slices.slice(1, -1).map((slice) => slice.text),
          end.source.text.slice(0, to),
        ].join("");
  const text = exact.trim();
  if (!text) return;
  return {
    text,
    offset: scalarOffset(start.source, from) + [...exact.slice(0, exact.indexOf(text))].length,
  };
}

// A folded previous-threads table draws its rows out of source order. A selection that ends
// inside it maps to one source range only when the rows it covers form one unbroken run in
// source order, from the row it starts in, or the first row when it starts outside the rows, to
// the row it ends in, or the last row when it ends outside them. Rows folded closed are never
// covered. A selection around the whole table quotes it.
function followsSourceRows(range: Range) {
  const around = parentElement(range.commonAncestorContainer);
  const tables = [
    ...(around?.closest("table") ? [around.closest("table")!] : []),
    ...(around?.querySelectorAll("table") ?? []),
  ];
  return tables.every((table) => {
    if (
      !table.querySelector(":scope > .report-thread-control") ||
      (!table.contains(range.startContainer) && !table.contains(range.endContainer))
    )
      return true;
    const rows = [...table.querySelectorAll<HTMLElement>(":scope > tbody > tr[data-report-row]")];
    const covered = rows
      .filter(
        (row) =>
          range.intersectsNode(row) && !row.closest(".report-thread-settled:not([data-open])"),
      )
      .map((row) => Number(row.dataset.reportRow))
      .sort((a, b) => a - b);
    const rowOf = (node: Node) => {
      const row = rows.find((candidate) => candidate.contains(node));
      return row && Number(row.dataset.reportRow);
    };
    const start = rowOf(range.startContainer);
    const end = rowOf(range.endContainer);
    const first = covered[0];
    const last = covered.at(-1);
    if (first === undefined || last === undefined) return start === undefined && end === undefined;
    return (
      last - first === covered.length - 1 &&
      (start === undefined ? first === 0 : start === first) &&
      (end === undefined ? last === rows.length - 1 : end === last)
    );
  });
}

export function scalarOffset(slice: ReportText, local: number) {
  return slice.scalar + [...slice.text.slice(0, Math.max(0, local))].length;
}

function sourcePoint(source: string, range: Range, boundary: "start" | "end") {
  const container = boundary === "start" ? range.startContainer : range.endContainer;
  const offset = boundary === "start" ? range.startOffset : range.endOffset;
  const leaf =
    container instanceof Text
      ? container
      : boundaryLeaf(container.childNodes[boundary === "start" ? offset : offset - 1], boundary);
  if (!leaf || !range.intersectsNode(leaf)) return;
  // A marker holds one source text, which a table may split at wrap points (breakLongNames).
  const marker = leaf.parentElement?.closest<HTMLElement>("[data-report-text-start]");
  if (
    !marker ||
    leaf.parentNode !== marker ||
    ![...marker.childNodes].every((node) => node instanceof Text || isWrapPoint(node))
  )
    return;
  const sourceStart = Number(marker.dataset.reportTextStart);
  const sourceEnd = Number(marker.dataset.reportTextEnd);
  if (!Number.isSafeInteger(sourceStart) || !Number.isSafeInteger(sourceEnd)) return;
  const rendered = marker.textContent;
  let length = container === leaf ? offset : boundary === "start" ? 0 : leaf.length;
  for (let piece = leaf.previousSibling; piece; piece = piece.previousSibling)
    length += piece.textContent?.length ?? 0;
  if (source.slice(sourceStart, sourceEnd) === rendered) return sourceStart + length;
  if (length === 0) return sourceStart;
  if (length === rendered.length) return sourceEnd;
}

function boundaryLeaf(
  branch: ChildNode | null | undefined,
  direction: "start" | "end",
): Text | undefined {
  // A boundary beside a wrap point is the edge of the text on its other side.
  if (branch && isWrapPoint(branch))
    return boundaryLeaf(
      direction === "start" ? branch.nextSibling : branch.previousSibling,
      direction,
    );
  if (branch instanceof Text) return branch;
  if (!branch) return;
  const walker = document.createTreeWalker(branch, NodeFilter.SHOW_TEXT);
  const text = direction === "start" ? walker.nextNode() : walker.lastChild();
  return text instanceof Text ? text : undefined;
}

function isWrapPoint(node: Node) {
  return node instanceof HTMLElement && node.localName === "wbr";
}

export function parentElement(node: Node) {
  return node instanceof Element ? node : node.parentElement;
}

export function reportParagraphQuote(paragraph: HTMLElement, source: string, block: ReportBlock) {
  const start = Number(paragraph.dataset.reportParagraphStart);
  const end = Number(paragraph.dataset.reportParagraphEnd);
  if (
    !Number.isSafeInteger(start) ||
    !Number.isSafeInteger(end) ||
    start < 0 ||
    start < block.offset ||
    start >= end ||
    end > block.end ||
    end > source.length
  )
    return;
  const exact = source.slice(start, end);
  const text = exact.trim();
  return text ? { text, offset: start + exact.indexOf(text) } : undefined;
}

function authoredFinding(
  nodes: readonly RootContent[],
  source: string,
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
): AuthoredFinding | undefined {
  const [heading, ...rest] = nodes;
  if (heading?.type !== "element" || heading.tagName !== "h3") return;
  const name = text(heading).trim();
  const match = findingHeading.exec(name);
  const id = match?.[1];
  const card = cardMetadata(rest[0]);
  if (!id || !match || !card) return;
  const change = changes?.get(id);
  const lead = [...stripPrefix(heading, match[0].length, source)];
  const fields: { label: string; nodes: RootContent[] }[] = [];
  let rank = -1;
  for (const node of rest.slice(1)) {
    const paragraph = node.type === "element" && node.tagName === "p" ? node : undefined;
    const first = paragraph?.children[0];
    const bold = first?.type === "element" && first.tagName === "strong" ? text(first) : "";
    const comment = paragraph?.children.length === 1 && proposedComment.lead.test(bold);
    const label = comment ? proposedComment.label : /^(.+):$/.exec(bold)?.[1];
    const order = comment
      ? proposedComment.order
      : label === undefined
        ? undefined
        : fieldOrder.get(label);
    if (paragraph && label && order !== undefined && order > rank) {
      rank = order;
      fields.push({
        label,
        nodes: comment
          ? [paragraph]
          : paragraph.children.length > 1
            ? [{ ...paragraph, children: paragraph.children.slice(1) }]
            : [],
      });
    } else {
      (fields.at(-1)?.nodes ?? lead).push(node);
    }
  }

  return {
    id,
    ...(change ? { change } : {}),
    ...card,
    header: authored(lead),
    fields: fields.map(({ label, nodes }) => ({ label, nodes: authored(nodes) })),
  };
}

function cardMetadata(node: RootContent | undefined) {
  if (node?.type !== "element" || node.tagName !== "p" || node.children.length !== 1) return;
  const [only] = node.children;
  const match =
    only?.type === "element" && only.tagName === "strong" ? cardLine.exec(text(only)) : null;
  if (!match) return;
  const [, severity = "", category = "", blocking] = match;
  return { severity, category, blocking: blocking === "Blocking" };
}

// Drops the `F-01 · ` prefix from the heading's first text node, which ends there when the
// title begins with code, emphasis or a link.
function stripPrefix(heading: Element, length: number, source: string): RootContent[] {
  const [first, ...others] = heading.children;
  const start = first?.position?.start.offset;
  if (
    first?.type !== "text" ||
    !first.position ||
    start === undefined ||
    length > first.value.length ||
    source.slice(start, start + length) !== first.value.slice(0, length)
  )
    return [heading];
  const remainder =
    length < first.value.length
      ? [
          {
            ...first,
            value: first.value.slice(length),
            position: {
              start: { ...first.position.start, offset: start + length },
              end: first.position.end,
            },
          },
        ]
      : [];
  return [{ ...heading, children: [...remainder, ...others] }];
}

// Report content with its source markers and its tables' wrap points.
function authored(nodes: readonly RootContent[]) {
  return annotate(nodes).map(breakLongNames);
}

export function annotate(nodes: readonly RootContent[]) {
  return nodes.map(annotateNode);
}

function annotateNode(node: RootContent): RootContent {
  if (node.type === "element") {
    if (node.tagName === "pre" || node.tagName === "svg") return node;
    const start = node.position?.start.offset;
    const end = node.position?.end.offset;
    return {
      ...node,
      ...(node.tagName === "p" && start !== undefined && end !== undefined
        ? {
            properties: {
              ...node.properties,
              dataReportParagraphStart: start,
              dataReportParagraphEnd: end,
            },
          }
        : {}),
      children: node.children.map(annotateNode) as Element["children"],
    };
  }
  const start = node.position?.start.offset;
  const end = node.position?.end.offset;
  if (
    node.type !== "text" ||
    !node.value.trim() ||
    start === undefined ||
    end === undefined ||
    end <= start
  )
    return node;
  return {
    type: "element",
    tagName: "span",
    properties: { dataReportTextStart: start, dataReportTextEnd: end },
    children: [node],
  };
}
