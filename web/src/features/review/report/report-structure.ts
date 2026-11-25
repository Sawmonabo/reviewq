import type { Element, ElementContent, RootContent, Text } from "hast";
import type { Descriptor, Verdict } from "../../../api/generated/contracts/reports";
import type { FindingConclusion } from "../../../api/generated/contracts/common";

export type AcceptedFindingChange = Extract<Descriptor, { kind: "change" }>;
export const outcomeLabels: Readonly<Record<FindingConclusion, string>> = {
  fixed: "Fixed",
  clarified_withdrawn: "Clarified and withdrawn",
  superseded: "Superseded",
  partly_addressed: "Partly addressed",
  still_present: "Still present",
  unverified: "Unverified",
};
const retiringOutcomes = new Set([
  outcomeLabels.fixed,
  outcomeLabels.clarified_withdrawn,
  outcomeLabels.superseded,
]);
const outcomeNames = new Set(Object.values(outcomeLabels));
// The composed verdict line, which is left out for an unspecified verdict.
const verdictLines: ReadonlyMap<string, Verdict> = new Map([
  ["APPROVE", "approve"],
  ["REQUEST CHANGES", "request_changes"],
  ["BLOCKED", "blocked"],
]);
const threadSection = "Our previous threads";
const threadColumns = ["Finding", "Outcome and verification", "Thread or evidence"];
const countLabel =
  "(?:P[0-3](?: findings?)?|(?:nonblocking|blocking)(?: findings?)?|(?:functional|quality|scale)(?: findings?)?)";
const countToken = new RegExp(`(?:\\d+\\s+${countLabel}|${countLabel}\\s*:\\s*\\d+)`, "gi");
// The four cover lines of a composed version, which the formatted view leaves out.
const coverLabels = new Set(["Scope:", "Reviewed source:", "Reviewed target:", "Checked at:"]);

export function reportStructure(
  nodes: readonly RootContent[],
  source: string,
  body?: { readonly previousThreads: boolean },
): readonly RootContent[] {
  const article = nodes.filter((node) => node.type !== "text" || node.value.trim());
  const output: RootContent[] = [];
  const firstSection = body ? 0 : article.findIndex(startsReportBody);
  const introEnd = firstSection < 0 ? article.length : firstSection;
  let previousThreads = body?.previousThreads ?? false;

  for (let index = 0; index < article.length; index++) {
    const node = article[index];
    if (node?.type !== "element") {
      if (node) output.push(node);
      continue;
    }
    const next = article[index + 1];
    const previous = article[index - 1];
    if (index < introEnd) {
      const tone = authoredVerdict(node);
      if (tone) {
        const rationale = paragraph(next) && !isCounts(reportText(next)) ? next : undefined;
        output.push(verdict(node, tone, rationale));
        if (rationale) index++;
        continue;
      }

      if (paragraph(node) && isCounts(reportText(node))) {
        output.push(authoredCounts(node, source));
        continue;
      }

      if (node.tagName === "h1") {
        output.push(styled(node, "report-title"));
        continue;
      }
      if (paragraph(node) && next?.type === "element" && next.tagName === "h1") {
        output.push(styled(node, "report-identity"));
        continue;
      }
      if (isCoverLine(node)) continue;
      if (paragraph(node) && previous?.type === "element" && previous.tagName === "h1") {
        output.push(styled(node, "report-lede"));
        continue;
      }
    }
    if (node.tagName === "h2") previousThreads = isThreadSection(reportText(node).trim());
    output.push(node.tagName === "table" ? reportTable(node, previousThreads) : node);
  }
  return output;
}

export function startsReportBody(node: RootContent) {
  return node.type === "element" && (/^h[23]$/.test(node.tagName) || node.tagName === "table");
}
export function isThreadSection(label: string) {
  return label === threadSection;
}

// A paragraph that is only the bold verdict, such as `**REQUEST CHANGES**`.
function authoredVerdict(node: Element) {
  const [only] = node.children;
  if (!paragraph(node) || node.children.length !== 1) return;
  return only?.type === "element" && only.tagName === "strong"
    ? verdictLines.get(reportText(only))
    : undefined;
}

function verdict(node: Element, tone: Verdict, rationale: Element | undefined): Element {
  const label = node.children[0];
  return {
    ...element("div", "report-verdict", [
      element(
        "div",
        "report-verdict-label",
        label?.type === "element" ? [{ ...label, tagName: "span" }] : [],
      ),
      ...(rationale ? [styled(rationale, "report-verdict-reason")] : []),
    ]),
    properties: { className: ["report-verdict"], dataReportVerdict: tone },
    ...(node.position && { position: node.position }),
  };
}

function isCoverLine(node: Element) {
  const first = node.children[0];
  return (
    node.tagName === "p" &&
    first?.type === "element" &&
    first.tagName === "strong" &&
    coverLabels.has(reportText(first))
  );
}

function isCounts(text: string) {
  return (
    [...text.matchAll(countToken)].length > 0 && /^[\s·•,|/]*$/.test(text.replace(countToken, ""))
  );
}

function authoredCounts(node: Element, source: string) {
  const child = node.children[0];
  const first =
    child?.type === "element" && child.tagName === "strong" && child.children.length === 1
      ? child.children[0]
      : child;
  const begin = first?.position?.start.offset;
  const end = first?.position?.end.offset;
  if (
    node.children.length !== 1 ||
    first?.type !== "text" ||
    begin === undefined ||
    end === undefined ||
    source.slice(begin, end) !== first.value
  )
    return styled(node, "report-counts");
  const children: ElementContent[] = [];
  let offset = 0;
  for (const entry of first.value.matchAll(countToken)) {
    const start = entry.index;
    if (start > offset)
      children.push(element("span", "report-count-separator", [textRange(first, offset, start)]));
    const label = entry[0];
    const digits = /^\d+/.exec(label) ?? /\d+$/.exec(label);
    if (!digits) continue;
    const numberStart = start + digits.index;
    const numberEnd = numberStart + digits[0].length;
    offset = start + label.length;
    const parts: ElementContent[] = [];
    if (numberStart > start) parts.push(textRange(first, start, numberStart));
    parts.push(
      element("strong", "report-count-amount", [textRange(first, numberStart, numberEnd)]),
    );
    if (numberEnd < offset) parts.push(textRange(first, numberEnd, offset));
    children.push({
      ...element("span", "report-count", parts),
      properties: {
        className: ["report-count"],
        dataReportCount: countTone(label),
        dataReportCountValue: Number(digits[0]),
      },
    });
  }
  if (offset < first.value.length) children.push(textRange(first, offset, first.value.length));
  return { ...styled(node, "report-counts"), children };
}

function countTone(label: string) {
  return /\bnonblocking\b/i.test(label)
    ? "nonblocking"
    : /\bblocking\b/i.test(label)
      ? "blocking"
      : (/\bP[0-3]\b/i.exec(label)?.[0].toLowerCase() ?? "neutral");
}

function reportTable(node: Element, previousThreads: boolean): Element {
  const head = node.children.find((child) => child.type === "element" && child.tagName === "thead");
  const rows =
    head?.type === "element" ? head.children.filter((child) => child.type === "element") : [];
  const cells = rows[0]?.children.filter((child) => child.type === "element");
  const headings = cells?.map((cell) => reportText(cell).trim());
  if (
    rows.length === 1 &&
    headings?.length === 2 &&
    headings[0]?.toLowerCase() === "before" &&
    headings[1]?.toLowerCase() === "after" &&
    twoColumnBody(node)
  )
    return styled(node, "report-before-after");
  if (!previousThreads) return node;
  const table = styled(node, "report-previous-threads");
  if (
    rows.length !== 1 ||
    headings?.length !== threadColumns.length ||
    headings.some((label, index) => label !== threadColumns[index])
  )
    return table;
  return {
    ...table,
    children: table.children.map((body) =>
      body.type === "element" && body.tagName === "tbody"
        ? {
            ...body,
            children: body.children.map((row) =>
              row.type === "element" && row.tagName === "tr"
                ? { ...row, children: toneOutcome(row.children) }
                : row,
            ),
          }
        : body,
    ),
  };
}

function twoColumnBody(table: Element) {
  const body = table.children.find(
    (child) => child.type === "element" && child.tagName === "tbody",
  );
  if (body?.type !== "element") return false;
  const rows = body.children.filter((child) => child.type === "element");
  return (
    rows.length > 0 &&
    rows.every((row) => {
      const cells = row.children.filter((child) => child.type === "element");
      return (
        row.tagName === "tr" &&
        cells.length === 2 &&
        cells.every(
          (cell) =>
            cell.tagName === "td" &&
            Number(cell.properties.colSpan ?? 1) === 1 &&
            Number(cell.properties.rowSpan ?? 1) === 1,
        )
      );
    })
  );
}

// The outcome cell begins with the outcome in bold, such as `**Fixed.**`. Colour it as
// retiring or still needing attention.
function toneOutcome(cells: ElementContent[]) {
  const outcome = cells.filter((cell) => cell.type === "element" && cell.tagName === "td")[1];
  const lead = outcome?.type === "element" ? outcome.children[0] : undefined;
  if (outcome?.type !== "element" || lead?.type !== "element" || lead.tagName !== "strong")
    return cells;
  const name = reportText(lead).trim().replace(/\.$/, "");
  if (!outcomeNames.has(name)) return cells;
  const toned: Element = {
    ...lead,
    properties: {
      ...lead.properties,
      dataReportThreadStatus: retiringOutcomes.has(name) ? "confirmed" : "attention",
    },
  };
  return cells.map((cell) =>
    cell === outcome ? { ...outcome, children: [toned, ...outcome.children.slice(1)] } : cell,
  );
}

function paragraph(node: RootContent | undefined): node is Element {
  return node?.type === "element" && node.tagName === "p";
}

function styled(node: Element, name: string): Element {
  return { ...node, properties: { ...node.properties, className: [name] } };
}

function element(tagName: string, name: string, children: ElementContent[]): Element {
  return { type: "element", tagName, properties: name ? { className: [name] } : {}, children };
}

function literal(value: string): Text {
  return { type: "text", value };
}

function textRange(node: Text, start: number, end: number): Text {
  const value = node.value.slice(start, end);
  const position = node.position;
  const offset = position?.start.offset;
  if (!position || offset === undefined) return literal(value);
  return {
    type: "text",
    value,
    position: {
      start: { ...position.start, offset: offset + start },
      end: { ...position.end, offset: offset + end },
    },
  };
}

export function reportText(node: RootContent): string {
  if (node.type === "text") return node.value;
  return "children" in node ? node.children.map(reportText).join("") : "";
}
