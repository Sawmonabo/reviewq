import type { DrawingHint } from "../../../api/generated/contracts/common";
import type { Element, Root, RootContent } from "hast";
import type {
  Descriptor,
  ReportSummary,
  SourceOrigin,
} from "../../../api/generated/contracts/reports";
import { isRetired, reportStructure, reportText as text } from "./report-structure";
import type { AcceptedFindingChange } from "./report-structure";

type Field = { readonly label: string; readonly nodes: readonly RootContent[] };
type AuthoredFinding = {
  readonly id: string;
  readonly title: string;
  readonly severity?: string;
  readonly category?: string;
  readonly blocking?: boolean;
  readonly change?: AcceptedFindingChange;
  // Set on a card drawn from the findings ledger. `earlier` is where a carried card was authored.
  readonly unanchored?: {
    readonly earlier?: { readonly origin: SourceOrigin; readonly offset: number };
  };
  readonly header: readonly RootContent[];
  readonly fields: readonly Field[];
};
export type ReportBlock = GroupPosition & {
  readonly end: number;
  readonly nodes: readonly RootContent[];
  readonly name?: string;
  readonly finding?: AuthoredFinding;
};

type Finding = Extract<Descriptor, { kind: "finding" }>;

const previousThreads =
  /^(?:(?:our )?previous threads|prior(?: review)? threads|discussion outcomes)$/i;
const findingHeading = /^(F-\d{1,16})(?=$|[\s:.–—-])\s*(?:[·:.–—-]\s*)?(?:(P[0-3])\s*[·–—-]\s*)?/;
const severityPrefix = /^(P[0-3])\s*[·–—-]\s*/;
const fieldName =
  /^(Issue|Why it matters|Confirmed|Proposed comment|Location|Evidence|Suggested fix)(?=\s*(?:[:.—]|$))/i;

export type GroupPosition = {
  readonly sourceOrder: number;
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

export function inDisplayOrder<T extends GroupPosition>(
  groups: readonly T[],
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
) {
  const retired = groups.filter(
    (group) => group.findingId && isRetired(changes?.get(group.findingId)),
  );
  if (!retired.length) return groups;
  const active = groups.filter((group) => !retired.includes(group));
  const next = afterPreviousThreads(active);
  active.splice(next < 0 ? active.length : next, 0, ...retired);
  return active;
}

export function carriedPosition(
  groups: readonly GroupPosition[],
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
) {
  const threads = afterPreviousThreads(groups);
  if (threads >= 0) return threads;
  for (let index = groups.length - 1; index >= 0; index--) {
    const id = groups[index]!.findingId;
    if (id && !isRetired(changes?.get(id))) return index + 1;
  }
  return groups.length;
}

export function unanchoredBlock(
  item: Finding,
  change: AcceptedFindingChange | undefined,
): ReportBlock {
  const details = item.details;
  const blocking = item.disposition?.blocking ?? item.blocking;
  const location = details?.location;
  // A title restated from a heading can still begin with its ID and priority.
  const heading = findingHeading.exec(item.title);
  const own = heading?.[1] === item.findingId ? heading : null;
  let title = own ? item.title.slice(own[0].length) : item.title;
  const priority = own?.[2] ? undefined : severityPrefix.exec(title);
  if (priority) title = title.slice(priority[0].length);
  title = title.trim() || item.title;
  const severity = item.severity || own?.[2] || priority?.[1];
  const origin = item.source?.origin;
  const fields: Field[] = details
    ? [
        ...(location
          ? [
              {
                label: "Location",
                nodes: [
                  hast("p", [
                    hast("code", [
                      {
                        type: "text",
                        value: `${location.path}${location.line ? `:${location.line}` : ""}`,
                      },
                    ]),
                  ]),
                ],
              },
            ]
          : []),
        { label: "Why it matters", nodes: plainParagraphs(details.consequence) },
        {
          label: details.evidence.kind === "gap" ? "Evidence gap" : "Evidence",
          nodes: plainParagraphs(details.evidence.text),
        },
      ]
    : [];
  return {
    offset: -1,
    sourceOrder: -1,
    end: -1,
    nodes: [],
    findingId: item.findingId,
    finding: {
      id: item.findingId,
      title,
      ...(severity ? { severity } : {}),
      ...(blocking !== null ? { blocking } : {}),
      ...(change ? { change } : {}),
      unanchored: item.carriedFrom
        ? {
            earlier:
              origin && item.source
                ? { origin, offset: item.source.offset }
                : { origin: item.carriedFrom, offset: 0 },
          }
        : {},
      header: [hast("h3", [{ type: "text", value: title }])],
      fields,
    },
  };
}

function afterPreviousThreads(groups: readonly GroupPosition[]) {
  const previous = groups.findIndex((group) => group.name && previousThreads.test(group.name));
  if (previous < 0) return -1;
  const next = groups.findIndex(
    (group, index) => index > previous && (group.name || group.findingId),
  );
  return next < 0 ? groups.length : next;
}

function plainParagraphs(value: string): RootContent[] {
  return value
    .split(/\n\s*\n/)
    .filter((part) => part.trim())
    .map((part) =>
      hast(
        "p",
        part
          .trim()
          .split("\n")
          .flatMap((line, index): Element["children"] =>
            index
              ? [hast("br", []), { type: "text", value: line }]
              : [{ type: "text", value: line }],
          ),
      ),
    );
}

function hast(tagName: string, children: Element["children"]): Element {
  return { type: "element", tagName, properties: {}, children };
}

export function reportBlocks(
  tree: Root,
  source: string,
  report: ReportSummary,
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
  body?: { readonly previousThreads: boolean },
): readonly ReportBlock[] {
  const groups = groupReportItems(
    reportStructure(tree.children, source, report, changes, body),
    (node) => {
      const level =
        node.type === "element" && /^h[1-3]$/.test(node.tagName)
          ? Number(node.tagName[1])
          : undefined;
      return {
        ...(level && { level, label: text(node).trim() }),
        ...(node.position?.start.offset !== undefined && { offset: node.position.start.offset }),
      };
    },
  );
  return inDisplayOrder(
    groups.map((group, index) => {
      const finding = authoredFinding(group.items, source, changes);
      return {
        offset: group.offset,
        sourceOrder: index,
        end: groups[index + 1]?.offset ?? source.length,
        nodes: finding ? [] : annotate(group.items),
        ...(group.name && { name: group.name }),
        ...(finding && { finding, findingId: finding.id }),
      };
    }),
    changes,
  );
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
  if (!start || !end || !range.toString().trim()) return;
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
  const marker = leaf.parentElement?.closest<HTMLElement>("[data-report-text-start]");
  if (!marker || marker.firstChild !== leaf || marker.lastChild !== leaf) return;
  const sourceStart = Number(marker.dataset.reportTextStart);
  const sourceEnd = Number(marker.dataset.reportTextEnd);
  if (!Number.isSafeInteger(sourceStart) || !Number.isSafeInteger(sourceEnd)) return;
  const rendered = leaf.data;
  const length = container === leaf ? offset : boundary === "start" ? 0 : rendered.length;
  if (source.slice(sourceStart, sourceEnd) === rendered) return sourceStart + length;
  if (length === 0) return sourceStart;
  if (length === rendered.length) return sourceEnd;
}

function boundaryLeaf(branch: ChildNode | undefined, direction: "start" | "end") {
  if (branch instanceof Text) return branch;
  if (!branch) return;
  const walker = document.createTreeWalker(branch, NodeFilter.SHOW_TEXT);
  const text = direction === "start" ? walker.nextNode() : walker.lastChild();
  return text instanceof Text ? text : undefined;
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
  if (!id || !match) return;
  const title = name.slice(match[0].length).trim();
  const change = changes?.get(id);
  let severity = match[2];
  let category: string | undefined;
  let blocking: boolean | undefined;
  const lead = [...stripPrefix(heading, match[0].length, source)];
  const fields: { label: string; nodes: RootContent[] }[] = [];
  for (const node of rest) {
    const label = text(node).trim();
    if (node.type === "element" && node.tagName === "p" && fields.length === 0) {
      const severityLine = /^Severity:\s*([^\n]{1,64})$/i.exec(label)?.[1]?.trim();
      const blockingLine = /^Blocking:\s*(true|yes|false|no)[.]?$/i.exec(label)?.[1]?.toLowerCase();
      if (severityLine) {
        severity = severityLine;
        continue;
      }
      if (blockingLine) {
        blocking = blockingLine === "true" || blockingLine === "yes";
        continue;
      }
      const first = node.children[0];
      const emphasis = first?.type === "element" && first.tagName === "strong";
      const metadata =
        /^(?:(P[0-3])\s*·\s*)?([^·\n]{1,80}?)\s*·\s*(nonblocking|blocking)[.]?$/i.exec(
          emphasis ? text(first).trim() : label,
        );
      if (metadata?.[2] && metadata[3]) {
        severity ??= metadata[1];
        category = metadata[2];
        blocking ??= metadata[3].toLowerCase() === "blocking";
        if (emphasis && node.children.length > 1)
          lead.push({ ...node, children: node.children.slice(1) });
        continue;
      }
    }
    const first = node.type === "element" && node.tagName === "p" ? node.children[0] : undefined;
    const headingField = node.type === "element" && /^h[4-6]$/.test(node.tagName);
    const labelNode = headingField
      ? node
      : first?.type === "element" && first.tagName === "strong"
        ? first
        : undefined;
    const field = labelNode ? fieldName.exec(text(labelNode).trim()) : undefined;
    if (field?.[1]) {
      const fieldLabel = field[1];
      const content: RootContent[] = [];
      const inline =
        labelNode &&
        text(labelNode)
          .trim()
          .slice(fieldLabel.length)
          .replace(/^[:.\s—]+/, "");
      if (inline) content.push(node);
      else if (!headingField && node.type === "element" && node.children.length > 1)
        content.push({ ...node, children: node.children.slice(1) });
      fields.push({ label: fieldLabel, nodes: content });
    } else {
      (fields.at(-1)?.nodes ?? lead).push(node);
    }
  }

  return {
    id,
    title,
    ...(category ? { category } : {}),
    ...(change ? { change } : {}),
    ...(severity ? { severity } : {}),
    ...(blocking !== undefined ? { blocking } : {}),
    header: annotate(lead),
    fields: fields.map(({ label, nodes }) => ({ label, nodes: annotate(nodes) })),
  };
}

function stripPrefix(heading: Element, length: number, source: string): RootContent[] {
  const first = heading.children[0];
  if (first?.type !== "text" || length >= first.value.length) return [heading];
  const start = first.position?.start.offset;
  const end = first.position?.end.offset;
  if (
    start === undefined ||
    end === undefined ||
    source.slice(start, end) !== first.value ||
    !first.position
  )
    return [heading];
  return [
    {
      ...heading,
      children: [
        {
          ...first,
          value: first.value.slice(length),
          position: {
            start: { ...first.position.start, offset: start + length },
            end: first.position.end,
          },
        },
        ...heading.children.slice(1),
      ],
    },
  ];
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
