// Runs the browser reader over a composed report and checks that it recognises every part
// the layout writes. Expectations come from the Markdown's structure, not from the layout's
// words, so a rewording on either the composer or the reader side fails.
export function verifyReportLayout({ parseMarkdown, wholeDocument, reportMarkdown }, markdown) {
  const tree = parseMarkdown(markdown, undefined, reportMarkdown);
  expect(tree, "parses the composed report");
  const reader = wholeDocument(markdown, tree, null, null);
  const blocks = reader.groups.map((_, index) => reader.prepared(index)?.block);
  expect(blocks.length > 1 && blocks.every(Boolean), "groups the report into blocks");

  const lines = markdown.split("\n");
  const bodyStart = lines.findIndex((line) => line.startsWith("## "));
  expect(bodyStart > 0, "finds the report body");
  const cover = lines.slice(0, bodyStart);
  const intro = blocks[0].nodes;

  const verdicts = cover.flatMap((line) => /^\*\*([^*]+)\*\*$/.exec(line)?.slice(1) ?? []);
  const verdict = find(intro, (node) => node.properties?.dataReportVerdict);
  expect(
    verdicts.length === 1 &&
      verdict?.properties.dataReportVerdict === verdicts[0].toLowerCase().replaceAll(" ", "_") &&
      text(verdict).includes(verdicts[0]),
    "draws the verdict block",
  );

  const countLines = cover.filter((line) => {
    const parts = line.split(" · ");
    return parts.length === 6 && parts.every((part) => /^[^:]+: \d+$/.test(part));
  });
  const counts = countLines[0]?.split(" · ").map((part) => Number(part.split(": ")[1]));
  const row = find(intro, (node) => node.properties?.className?.includes("report-counts"));
  const drawn = row?.children.filter(
    (child) => child.properties?.dataReportCountValue !== undefined,
  );
  expect(
    countLines.length === 1 &&
      drawn?.length === 6 &&
      drawn.every((count, index) => count.properties.dataReportCountValue === counts[index]),
    "draws the counts row with its six counts",
  );

  const coverLabels = cover
    .slice(lines.indexOf(countLines[0]))
    .flatMap((line) => /^\*\*([^*]+:)\*\* /.exec(line)?.slice(1) ?? []);
  const introText = intro.map(text).join("");
  expect(
    coverLabels.length > 0 && coverLabels.every((label) => !introText.includes(label)),
    "recognises every cover line",
  );

  const cards = blocks.filter((block) => block.finding).map((block) => block.finding);
  const headings = lines.flatMap((line, index) => (line.startsWith("### ") ? [index] : []));
  expect(
    headings.length > 0 && cards.length === headings.length,
    "draws a card for every finding heading",
  );
  for (const [position, start] of headings.entries()) {
    const card = cards[position];
    const id = lines[start].slice(4).split(" ")[0];
    const end = lines.findIndex((line, index) => index > start && /^#{2,3} /.test(line));
    const body = lines.slice(start + 1, end < 0 ? lines.length : end);
    const [severity, category, blocking] =
      body
        .find((line) => line.trim())
        ?.replace(/^\*\*(.*)\*\*$/, "$1")
        .split(" · ") ?? [];
    const labels = body.flatMap((line) => /^\*\*([^*]+):\*\*/.exec(line)?.slice(1) ?? []);
    expect(
      card.id === id &&
        card.severity === severity &&
        card.category === category &&
        card.blocking === (blocking === "Blocking") &&
        labels.length > 0 &&
        card.fields.length === labels.length &&
        card.fields.every((field, index) => field.label === labels[index]),
      `reads card ${id} with its card line and every field`,
    );
  }
  for (const [index, tone] of drawn.map((count) => count.properties.dataReportCount).entries()) {
    const tally = cards.filter((card) =>
      tone === "blocking"
        ? card.blocking
        : tone === "nonblocking"
          ? !card.blocking
          : card.severity.toLowerCase() === tone,
    ).length;
    expect(tally === counts[index], `counts the ${tone} cards`);
  }

  const tableStart = lines.findIndex((line, index) => index > bodyStart && line.startsWith("|"));
  const tableLength = lines.slice(tableStart).findIndex((line) => !line.startsWith("|"));
  const section = lines
    .slice(0, tableStart)
    .findLast((line) => line.startsWith("## "))
    ?.slice(3);
  const threads = blocks.find((block) => block.name === section);
  const table = threads && find(threads.nodes, (node) => node.tagName === "table");
  const rows = table && find(table.children, (node) => node.tagName === "tbody")?.children;
  expect(
    tableStart > 0 &&
      tableLength > 2 &&
      table?.properties.className?.includes("report-previous-threads") &&
      rows?.filter((node) => node.tagName === "tr").length === tableLength - 2,
    "draws the previous-threads table",
  );
}

function find(nodes, accepts) {
  for (const node of nodes) {
    if (accepts(node)) return node;
    const found = node.children && find(node.children, accepts);
    if (found) return found;
  }
}

function text(node) {
  return node.type === "text" ? node.value : (node.children ?? []).map(text).join("");
}

function expect(condition, description) {
  if (!condition) throw new Error(`Report layout: the reader no longer ${description}.`);
}
