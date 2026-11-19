import type { Element, Root, RootContent } from "hast";
import type { ReportSummary } from "../../../api/generated/contracts/reports";
import { reportStructure, reportText as text } from "./report-structure";

type Field = { readonly label: string; readonly nodes: readonly RootContent[] };
type AuthoredFinding = {
  readonly id: string;
  readonly severity?: string;
  readonly blocking?: boolean;
  readonly header: readonly RootContent[];
  readonly fields: readonly Field[];
};
export type ReportBlock = {
  readonly offset: number;
  readonly end: number;
  readonly nodes: readonly RootContent[];
  readonly name?: string;
  readonly finding?: AuthoredFinding;
};

const fieldName =
  /^(Issue|Why it matters|Confirmed|Proposed comment|Location|Evidence|Suggested fix)(?=\s*(?:[:.—]|$))/i;

export function reportBlocks(tree: Root, source: string, report: ReportSummary): ReportBlock[] {
  const blocks: { offset: number; nodes: RootContent[]; name?: string }[] = [];
  let current = blocks.at(-1);
  let hasSection = false;
  for (const node of reportStructure(tree.children, source, report)) {
    const heading =
      node.type === "element" && (hasSection ? /^h[1-3]$/ : /^h[2-3]$/).test(node.tagName)
        ? text(node).trim()
        : "";
    if (
      !current ||
      heading ||
      (current.nodes.length >= 24 && node.position?.start.offset != null)
    ) {
      current = {
        offset: node.position?.start.offset ?? 0,
        nodes: [],
        ...(heading && node.type === "element" && node.tagName === "h2" ? { name: heading } : {}),
      };
      blocks.push(current);
    }
    if (heading) hasSection = true;
    current.nodes.push(node);
  }
  return blocks.map((block, index) => {
    const finding = authoredFinding(block.nodes, source);
    return {
      ...block,
      nodes: finding ? [] : annotate(block.nodes),
      end: blocks[index + 1]?.offset ?? source.length,
      ...(finding ? { finding } : {}),
    };
  });
}

export function selectionSource(
  range: Range,
  source: string,
  start: ReportBlock,
  end: ReportBlock,
): { readonly offset: number; readonly text: string } | undefined {
  if (!range.toString().trim()) return;
  const from = sourcePoint(source, range, "start");
  const to = sourcePoint(source, range, "end");

  if (
    from !== undefined &&
    to !== undefined &&
    start.offset <= from &&
    from < to &&
    to <= end.end
  ) {
    const exact = source.slice(from, to);
    const trimmed = exact.trim();
    if (trimmed) return { offset: from + exact.indexOf(trimmed), text: trimmed };
  }
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

function authoredFinding(
  nodes: readonly RootContent[],
  source: string,
): AuthoredFinding | undefined {
  const [heading, ...rest] = nodes;
  if (heading?.type !== "element" || heading.tagName !== "h3") return;
  const name = text(heading).trim();
  const match = /^(F-\d{1,16})(?=$|[\s:.–—-])\s*(?:[·:.–—-]\s*)?(?:(P[0-3])\s*[·–—-]\s*)?/.exec(
    name,
  );
  const id = match?.[1];
  if (!id || !match) return;
  let severity = match[2];
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
      const metadata =
        /^\s*(?:(P[0-3])\s+·\s*)?[^·\n]+\s+·\s*(nonblocking|blocking)(?:[.\s]|$)/i.exec(label);
      severity ??= metadata?.[1];
      if (blocking === undefined && metadata?.[2])
        blocking = metadata[2].toLowerCase() === "blocking";
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

function annotate(nodes: readonly RootContent[]) {
  return nodes.map(annotateNode);
}

function annotateNode(node: RootContent): RootContent {
  if (node.type === "element") {
    if (node.tagName === "pre" || node.tagName === "svg") return node;
    return { ...node, children: node.children.map(annotateNode) as Element["children"] };
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
