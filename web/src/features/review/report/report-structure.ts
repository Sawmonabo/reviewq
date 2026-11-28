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
// The composed verdict line, which is left out for an unspecified verdict.
const verdictLines: ReadonlyMap<string, Verdict> = new Map([
  ["APPROVE", "approve"],
  ["REQUEST CHANGES", "request_changes"],
  ["BLOCKED", "blocked"],
]);
const threadSection = "Our previous threads";
// The composed counts line and its six counts.
const countsLine = /^P0: \d+ · P1: \d+ · P2: \d+ · P3: \d+ · Blocking: \d+ · Nonblocking: \d+$/;
const countToken = /(?:P[0-3]|Blocking|Nonblocking): (\d+)/g;
// The four cover lines of a composed version, which the formatted view leaves out.
const coverLabels = new Set(["Scope:", "Reviewed source:", "Reviewed target:", "Checked at:"]);
// A thread outcome that needs nothing more from the reader. A resolved discussion is confirmed
// in tone too, but GitLab resolution alone does not settle a thread.
const settledLabels = "fixed(?: in code)?|clarified(?: and withdrawn)?|withdrawn|superseded";
const settledLabel = new RegExp(`^(?:${settledLabels})$`, "i");
const confirmedLabel = new RegExp(`^(?:${settledLabels}|resolved(?: in GitLab)?)$`, "i");

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
  return countsLine.test(text);
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
    const [label, digits = ""] = entry;
    offset = start + label.length;
    const numberStart = offset - digits.length;
    children.push({
      ...element("span", "report-count", [
        textRange(first, start, numberStart),
        element("strong", "report-count-amount", [textRange(first, numberStart, offset)]),
      ]),
      properties: {
        className: ["report-count"],
        dataReportCount: countTone(label),
        dataReportCountValue: Number(digits),
      },
    });
  }
  return { ...styled(node, "report-counts"), children };
}

function countTone(label: string) {
  return label.startsWith("Nonblocking")
    ? "nonblocking"
    : label.startsWith("Blocking")
      ? "blocking"
      : label.slice(0, 2).toLowerCase();
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
  const table = styled(node, "report-previous-threads");
  if (!statusColumns?.length) return table;
  const styledTable = {
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
  // GitLab resolution alone is not an outcome, so the fold reads every other status column.
  const outcomes = statusColumns.filter((index) => headings?.[index] !== "gitlab");
  // Hidden rows add no width, so only CSS-sized columns (up to three) stay put when rows open.
  // Paired with the previous-threads column widths in report.css; change both together.
  const columns = headings?.length ?? 0;
  return outcomes.length && columns <= 3
    ? foldSettled(styledTable, outcomes, columns)
    : styledTable;
}

// Draws the settled rows after the others, behind one control the reader opens. Each body
// row keeps its source order in `data-report-row`, which a selection must respect, and each
// settled cell holds its content in a wrapper the reader hides until browser find reaches it.
// The table's position keys the reader's fold state, so a table without one stays unfolded.
function foldSettled(table: Element, outcomes: number[], columns: number): Element {
  const index = table.children.findIndex(
    (child) => child.type === "element" && child.tagName === "tbody",
  );
  const body = table.children[index];
  if (body?.type !== "element" || !table.position) return table;
  const rows = body.children.filter(
    (row): row is Element => row.type === "element" && row.tagName === "tr",
  );
  const numbered = rows.map((row, order) => ({
    ...row,
    properties: { ...row.properties, dataReportRow: order },
  }));
  const settled = numbered.filter((row) => isSettled(row, outcomes));
  if (!settled.length) return table;
  const position = table.position;
  const part = (name: string, children: ElementContent[], properties = {}): Element => ({
    type: "element",
    tagName: "tbody",
    properties: { className: [name], ...properties },
    children,
    position,
  });
  const kept = numbered.filter((row) => !settled.includes(row));
  const folded = settled.map((row) => ({
    ...row,
    children: row.children.map((cell) =>
      cell.type === "element" && cell.tagName === "td"
        ? { ...cell, children: [element("div", "report-thread-cell", cell.children)] }
        : cell,
    ),
  }));
  return {
    ...table,
    children: [
      ...table.children.slice(0, index),
      ...(kept.length ? [{ ...body, children: kept }] : []),
      part("report-thread-control", [], {
        dataReportSettled: settled.length,
        dataReportColumns: columns,
      }),
      part("report-thread-settled", folded),
      ...table.children.slice(index + 1),
    ],
  };
}

// Every outcome cell is a settled label, or begins with one in bold, such as
// `**Fixed.** reason`.
function isSettled(row: Element, outcomes: number[]) {
  const cells = row.children.filter(
    (child): child is Element => child.type === "element" && child.tagName === "td",
  );
  return outcomes.every((column) => {
    const cell = cells[column];
    if (!cell) return false;
    const lead = cell.children.find((child) => child.type !== "text" || child.value.trim());
    const label =
      lead?.type === "element" && lead.tagName === "strong" ? reportText(lead) : reportText(cell);
    return settledLabel.test(label.trim().replace(/\.$/, ""));
  });
}

// A plain table of up to three columns takes the panel's width (report.css picks the same
// tables), so a long name in its body, such as a file path, may wrap after "/" or "_" at a
// `wbr` instead of inside a word. This runs on annotated nodes, so each source text keeps its
// one marker, which then holds the pieces and the wrap points between them.
export function breakLongNames(node: RootContent): RootContent {
  if (node.type !== "element" || node.tagName !== "table" || !fitsPanel(node)) return node;
  return {
    ...node,
    children: node.children.flatMap((part) =>
      part.type === "element" && part.tagName === "tbody" ? breakNames(part) : [part],
    ),
  };
}

function fitsPanel(table: Element) {
  const name = table.properties.className;
  if (
    Array.isArray(name) &&
    name.some((own) => own === "report-before-after" || own === "report-previous-threads")
  )
    return false;
  return table.children.every(
    (part) =>
      part.type !== "element" ||
      part.children.every(
        (row) =>
          row.type !== "element" ||
          row.children.filter((cell) => cell.type === "element").length <= 3,
      ),
  );
}

function breakNames(node: ElementContent): ElementContent[] {
  if (node.type === "element") return [{ ...node, children: node.children.flatMap(breakNames) }];
  if (node.type !== "text") return [node];
  const cuts = nameBreaks(node.value);
  if (!cuts.length) return [node];
  return [0, ...cuts].flatMap((cut, index, all) => [
    ...(index ? [element("wbr", "", [])] : []),
    literal(node.value.slice(cut, all[index + 1])),
  ]);
}

// Where a long name may wrap: after each "/" or "_" in a run of 24 or more characters without
// a space, so it never breaks inside a word. A shorter name moves to the next line whole.
function nameBreaks(text: string) {
  const breaks: number[] = [];
  for (const run of text.matchAll(/\S{24,}/gu))
    for (const mark of run[0].matchAll(/[/_](?!$)/g)) breaks.push(run.index + mark.index + 1);
  return breaks;
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

function styleStatuses(cells: ElementContent[], columns: number[]) {
  let column = 0;
  return cells.map((cell) => {
    if (cell.type !== "element" || cell.tagName !== "td") return cell;
    const index = column++;
    const label = reportText(cell).trim();
    if (!columns.includes(index)) return cell;
    const tone = confirmedLabel.test(label)
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
