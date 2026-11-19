import type { Element, ElementContent, RootContent, Text } from "hast";
import type { ReportSummary, Verdict } from "../../../api/generated/contracts/reports";

type ReportHeader = Pick<ReportSummary, "verdict" | "findingCount" | "changeCount">;
type Tone = "approve" | "request_changes" | "blocked" | "neutral";
const verdictLabels: Readonly<Record<Verdict, string>> = {
  approve: "Approve",
  request_changes: "Request changes",
  blocked: "Blocked",
  not_specified: "Verdict unspecified",
};
const threadSections =
  /^(?:(?:our )?previous threads|prior(?: review)? threads|resolved findings|retired findings|discussion outcomes)$/i;
const countLabel =
  "(?:P[0-3](?: findings?)?|(?:nonblocking|blocking)(?: findings?)?|(?:functional|quality|scale)(?: findings?)?)";
const countToken = new RegExp(`(?:\\d+\\s+${countLabel}|${countLabel}\\s*:\\s*\\d+)`, "gi");
const metadata = /^(?:publication|disposition|scope|reviewed (?:sha|revision)|coverage)\s*:/i;

export function reportStructure(
  nodes: readonly RootContent[],
  source: string,
  summary: ReportHeader,
): readonly RootContent[] {
  const article = nodes.filter((node) => node.type !== "text" || node.value.trim());
  const output: RootContent[] = [];
  let intro = true;
  let sawVerdict = false;
  let sawCounts = false;
  let previousThreads = false;

  function addSummary() {
    if (!sawVerdict && summary.verdict !== "not_specified") output.push(verdict(summary.verdict));
    if (!sawCounts) {
      const items = [
        [summary.findingCount, "indexed finding"],
        [summary.changeCount, "declared change"],
      ] as const;
      const counting = items.flatMap(([amount, label]) =>
        amount === null ? [] : [countBadge(String(amount), label + (amount === 1 ? "" : "s"))],
      );
      if (counting.length) output.push(element("div", "report-counts", counting));
    }
  }

  for (let index = 0; index < article.length; index++) {
    const node = article[index];
    if (node?.type !== "element") {
      if (node) output.push(node);
      continue;
    }
    const next = article[index + 1];
    const previous = article[index - 1];
    if (intro) {
      const recognized = authoredVerdict(node);
      if (recognized) {
        const { tone, label, inline } = recognized;
        const supplied = tone ?? summary.verdict;
        const preceding = inline.length > 0;
        const following =
          !preceding &&
          paragraph(next) &&
          !isMetadata(next) &&
          !isCounts(reportText(next)) &&
          !authoredVerdict(next);
        const rationale = preceding ? [element("p", "", inline)] : following ? [next] : [];
        const recorded =
          tone && tone !== summary.verdict && summary.verdict !== "not_specified"
            ? summary.verdict
            : undefined;
        output.push(verdict(supplied, label, rationale, recorded));
        if (following) index++;
        sawVerdict = true;
        continue;
      }

      if (paragraph(node) && isCounts(reportText(node))) {
        if (!sawVerdict && summary.verdict !== "not_specified") {
          output.push(verdict(summary.verdict));
          sawVerdict = true;
        }
        output.push(authoredCounts(node, source));
        sawCounts = true;
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
      if (isMetadata(node)) {
        addSummary();
        sawVerdict = sawCounts = true;
        output.push(styled(node, "report-metadata"));
        continue;
      }
      if (paragraph(node) && previous?.type === "element" && previous.tagName === "h1") {
        output.push(styled(node, "report-lede"));
        continue;
      }
      if (node.tagName === "h2" || node.tagName === "h3" || node.tagName === "table") {
        addSummary();
        intro = false;
      }
    }
    if (node.tagName === "h2") previousThreads = threadSections.test(reportText(node).trim());
    output.push(node.tagName === "table" ? reportTable(node, previousThreads) : node);
  }
  if (intro) addSummary();
  return output;
}

function authoredVerdict(node: Element):
  | {
      tone?: Tone;
      label: Element;
      inline: ElementContent[];
    }
  | undefined {
  const first = node.children[0];
  const label =
    node.tagName === "h2"
      ? node
      : paragraph(node) && first?.type === "element" && first.tagName === "strong"
        ? first
        : undefined;
  if (!label) return;
  const text = reportText(label).trim().replace(/:$/, "").toUpperCase();
  const inline = label === node ? [] : node.children.slice(1);
  const supplied = { label: { ...label, tagName: "span" }, inline };
  if (text === "VERDICT") return supplied;
  if (text === "APPROVE") return { ...supplied, tone: "approve" };
  if (text === "REQUEST CHANGES") return { ...supplied, tone: "request_changes" };
  if (/^BLOCKED(?:\s*[-–—]\s*INSUFFICIENT EVIDENCE)?$/.test(text))
    return { ...supplied, tone: "blocked" };
  if (/^(?:NEEDS CLARIFICATION|COMMENT ONLY|INCOMPLETE)$/.test(text))
    return { ...supplied, tone: "neutral" };
}

function verdict(
  tone: Verdict | Tone,
  authored?: Element,
  rationale: Element[] = [],
  recorded?: Verdict,
): Element {
  const usesReceipt =
    authored && reportText(authored).trim().replace(/:$/, "").toUpperCase() === "VERDICT";
  const label =
    usesReceipt && tone !== "not_specified" && tone !== "neutral"
      ? [styled(authored, "report-verdict-caption"), literal(verdictLabels[tone])]
      : authored
        ? [authored]
        : [literal(tone === "neutral" ? "Verdict" : verdictLabels[tone])];
  return {
    ...element("div", "report-verdict", [
      element("div", "report-verdict-label", label),
      ...rationale.map((node) => styled(node, "report-verdict-reason")),
      ...(recorded
        ? [
            element("p", "report-verdict-recorded", [
              literal(`Recorded verdict · ${verdictLabels[recorded]}`),
            ]),
          ]
        : []),
    ]),
    properties: { className: ["report-verdict"], dataReportVerdict: tone },
  };
}

function isMetadata(node: Element) {
  return node.tagName === "p" && metadata.test(reportText(node).trim());
}

function isCounts(text: string) {
  return (
    [...text.matchAll(countToken)].length > 0 && /^[\s·•,|/]*$/.test(text.replace(countToken, ""))
  );
}

function authoredCounts(node: Element, source: string) {
  const first = node.children[0];
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
    const digits =
      /\d+$/.exec(label) && /^\D/.test(label) ? /\d+$/.exec(label) : /^\d+/.exec(label);
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
      properties: { className: ["report-count"], dataReportCount: countTone(label) },
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

function countBadge(amount: string, label: string) {
  return element("span", "report-count", [
    element("strong", "report-count-amount", [literal(amount)]),
    literal(` ${label}`),
  ]);
}

function reportTable(node: Element, previousThreads: boolean): Element {
  const head = node.children.find((child) => child.type === "element" && child.tagName === "thead");
  const rows =
    head?.type === "element" ? head.children.filter((child) => child.type === "element") : [];
  const cells = rows[0]?.children.filter((child) => child.type === "element");
  const headings = cells?.map((cell) => reportText(cell).trim().toLowerCase());
  if (
    rows.length === 1 &&
    headings?.length === 2 &&
    headings[0] === "before" &&
    headings[1] === "after"
  )
    return styled(node, "report-before-after");
  if (!previousThreads) return node;
  const statusColumns = headings?.flatMap((label, index) =>
    /^(?:status|assessment|conclusion|outcome|gitlab)$/.test(label) ? [index] : [],
  );
  const table = styled(node, "report-previous-threads");
  if (!statusColumns?.length) return table;
  return {
    ...table,
    children: table.children.map((body) =>
      body.type === "element" && body.tagName === "tbody"
        ? {
            ...body,
            children: body.children.map((row) =>
              row.type === "element" && row.tagName === "tr"
                ? {
                    ...row,
                    children: styleStatuses(row.children, statusColumns),
                  }
                : row,
            ),
          }
        : body,
    ),
  };
}

function styleStatuses(cells: ElementContent[], columns: number[]) {
  let column = 0;
  return cells.map((cell) => {
    if (cell.type !== "element" || cell.tagName !== "td") return cell;
    if (!columns.includes(column++)) return cell;
    const label = reportText(cell).trim();
    const tone =
      /^(?:fixed(?: in code)?|resolved(?: in GitLab)?|clarified(?: and withdrawn)?|withdrawn|superseded)$/i.test(
        label,
      )
        ? "confirmed"
        : /^(?:still present|open|unresolved|partly addressed|unverified)$/i.test(label)
          ? "attention"
          : "neutral";
    return { ...cell, properties: { ...cell.properties, dataReportThreadStatus: tone } };
  });
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
