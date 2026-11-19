import type { Element, ElementContent, RootContent, Text } from "hast";
import type { Descriptor, ReportSummary, Verdict } from "../../../api/generated/contracts/reports";
import type { FindingConclusion } from "../../../api/generated/contracts/common";

type ReportHeader = Pick<ReportSummary, "verdict" | "findingCount" | "changeCount">;
export type AcceptedFindingChange = Extract<Descriptor, { kind: "change" }>;
type Tone = "approve" | "request_changes" | "blocked" | "neutral";
export const outcomeLabels: Readonly<Record<FindingConclusion, string>> = {
  fixed: "Fixed",
  clarified_withdrawn: "Clarified and withdrawn",
  superseded: "Superseded",
  partly_addressed: "Partly addressed",
  still_present: "Still present",
  unverified: "Unverified",
};
export function isRetired(change: AcceptedFindingChange | undefined) {
  return (
    change?.conclusion === "fixed" ||
    change?.conclusion === "clarified_withdrawn" ||
    change?.conclusion === "superseded"
  );
}
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
const coverMetadata =
  /^(?:scope|reviewed (?:source|target|sha|revision)|coverage|checked at|provenance)\s*:/i;
const contextMetadata = /^(?:publication|disposition)\s*:/i;

export function reportStructure(
  nodes: readonly RootContent[],
  source: string,
  summary: ReportHeader,
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
): readonly RootContent[] {
  const article = nodes.filter((node) => node.type !== "text" || node.value.trim());
  const output: RootContent[] = [];
  const firstSection = article.findIndex(
    (node) =>
      node.type === "element" &&
      (/^h[23]$/.test(node.tagName) || node.tagName === "table") &&
      !authoredVerdict(node),
  );
  const introEnd = firstSection < 0 ? article.length : firstSection;
  const introduction = article.slice(0, introEnd);
  const hasAuthoredVerdict = introduction.some(
    (node) => node.type === "element" && authoredVerdict(node),
  );
  const hasAuthoredCounts = introduction.some(
    (node) => paragraph(node) && isCounts(reportText(node)),
  );
  let addedVerdict = false;
  let addedCounts = false;
  let previousThreads = false;

  function addSummary() {
    if (!addedVerdict && !hasAuthoredVerdict && summary.verdict !== "not_specified") {
      output.push(verdict(summary.verdict));
      addedVerdict = true;
    }
    if (!addedCounts && !hasAuthoredCounts) {
      const items = [
        [summary.findingCount, "indexed finding"],
        [summary.changeCount, "declared change"],
      ] as const;
      const counting = items.flatMap(([amount, label]) =>
        amount === null ? [] : [countBadge(String(amount), label + (amount === 1 ? "" : "s"))],
      );
      if (counting.length) output.push(element("div", "report-counts", counting));
      addedCounts = true;
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
    if (index === introEnd) addSummary();
    if (index < introEnd) {
      const recognized = authoredVerdict(node);
      if (recognized) {
        const explicit =
          next?.type === "element" && !recognized.tone && recognized.inline.length === 0
            ? authoredVerdict(next)
            : undefined;
        const consumesLabel = explicit?.tone !== undefined;
        const { tone, label, inline } = consumesLabel ? explicit : recognized;
        if (consumesLabel) index++;
        const afterLabel = article[index + 1];
        const following =
          inline.length === 0 &&
          paragraph(afterLabel) &&
          !isMetadata(afterLabel) &&
          !isCounts(reportText(afterLabel)) &&
          !authoredVerdict(afterLabel);
        const rationale = inline.length
          ? [element("p", "", inline)]
          : following
            ? [afterLabel]
            : [];
        const recorded =
          tone && tone !== summary.verdict && summary.verdict !== "not_specified"
            ? summary.verdict
            : undefined;
        const card = verdict(tone ?? summary.verdict, label, rationale, recorded);
        const labelContainer = card.children[0];
        if (consumesLabel && labelContainer?.type === "element") {
          labelContainer.children.unshift(styled(recognized.label, "report-verdict-caption"));
        }
        if (node.position) card.position = node.position;
        output.push(card);
        if (following) index++;
        continue;
      }

      if (paragraph(node) && isCounts(reportText(node))) {
        addSummary();
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
      if (paragraph(node) && coverMetadata.test(reportText(node).trim())) {
        const provenance = [node];
        while (index + 1 < introEnd) {
          const candidate = article[index + 1];
          if (!paragraph(candidate) || !coverMetadata.test(reportText(candidate).trim())) break;
          provenance.push(candidate);
          index++;
        }
        output.push(
          element("details", "report-provenance", [
            element("summary", "", [literal("Scope and revision details")]),
            ...provenance.map((entry) => styled(entry, "report-metadata")),
          ]),
        );
        continue;
      }
      if (isMetadata(node)) {
        addSummary();
        output.push(styled(node, "report-metadata"));
        continue;
      }
      if (paragraph(node) && previous?.type === "element" && previous.tagName === "h1") {
        output.push(styled(node, "report-lede"));
        continue;
      }
    }
    if (node.tagName === "h2") previousThreads = threadSections.test(reportText(node).trim());
    output.push(node.tagName === "table" ? reportTable(node, previousThreads, changes) : node);
  }
  if (firstSection < 0) addSummary();
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
  const label = reportText(node).trim();
  return node.tagName === "p" && (contextMetadata.test(label) || coverMetadata.test(label));
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

function countBadge(amount: string, label: string) {
  return element("span", "report-count", [
    element("strong", "report-count-amount", [literal(amount)]),
    literal(` ${label}`),
  ]);
}

function reportTable(
  node: Element,
  previousThreads: boolean,
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
): Element {
  const head = node.children.find((child) => child.type === "element" && child.tagName === "thead");
  const rows =
    head?.type === "element" ? head.children.filter((child) => child.type === "element") : [];
  const cells = rows[0]?.children.filter((child) => child.type === "element");
  const headings = cells?.map((cell) => reportText(cell).trim().toLowerCase());
  if (
    rows.length === 1 &&
    headings?.length === 2 &&
    headings[0] === "before" &&
    headings[1] === "after" &&
    twoColumnBody(node)
  )
    return styled(node, "report-before-after");
  if (!previousThreads) return node;
  const statusColumns = headings?.flatMap((label, index) =>
    /^(?:status|assessment|conclusion|outcome(?: and verification)?|gitlab)$/.test(label)
      ? [index]
      : [],
  );
  const findingColumn = headings?.findIndex((label) => /^(?:finding|id)$/.test(label)) ?? -1;
  const outcomeColumn =
    headings?.findIndex((label) =>
      /^(?:assessment|conclusion|outcome(?: and verification)?)$/.test(label),
    ) ?? -1;
  const table = styled(node, "report-previous-threads");
  if (!statusColumns?.length && findingColumn < 0) return table;
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
                    children: styleStatuses(
                      row.children,
                      statusColumns ?? [],
                      findingColumn,
                      outcomeColumn,
                      changes,
                    ),
                  }
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

function styleStatuses(
  cells: ElementContent[],
  columns: number[],
  findingColumn: number,
  outcomeColumn: number,
  changes: ReadonlyMap<string, AcceptedFindingChange> | null | undefined,
) {
  const identity = cells.filter((cell) => cell.type === "element" && cell.tagName === "td")[
    findingColumn
  ];
  const id = identity && /^(F-\d+)\b/.exec(reportText(identity).trim())?.[1];
  const accepted = id ? changes?.get(id) : undefined;
  let column = 0;
  return cells.map((cell) => {
    if (cell.type !== "element" || cell.tagName !== "td") return cell;
    const index = column++;
    const label = reportText(cell).trim();
    if (accepted && index === (outcomeColumn >= 0 ? outcomeColumn : findingColumn)) {
      const badge = {
        ...element("span", "report-thread-outcome", [literal(outcomeLabels[accepted.conclusion])]),
        properties: {
          className: ["report-thread-outcome"],
          title: accepted.reason,
          dataReportThreadStatus: isRetired(accepted) ? "confirmed" : "attention",
        },
      };
      return {
        ...cell,
        children: outcomeColumn >= 0 ? [badge, ...cell.children] : [...cell.children, badge],
      };
    }
    if (!columns.includes(index)) return cell;
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
