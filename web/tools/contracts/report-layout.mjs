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

  const titleLine = cover.findIndex((line) => line.startsWith("# "));
  const identityLinks = links(cover.slice(0, titleLine).findLast((line) => line.trim()) ?? "");
  const identity = find(intro, (node) => node.properties?.className?.includes("report-identity"));
  expect(
    identityLinks.length > 1 && drawsLinks(identity, identityLinks),
    "draws the identity line with each of its links",
  );

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
  let linkedLocations = 0;
  let commentLeads = 0;
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
    // A field opens with `**Label:**`, or with a bold lead line `**Label — …:**` that the
    // field keeps as its first line.
    const opened = body.flatMap((line) => {
      const [, name] = /^\*\*([^*]+):\*\*/.exec(line) ?? [];
      return name ? [{ line, label: name.split(" — ")[0], lead: name.includes(" — ") }] : [];
    });
    expect(
      card.id === id &&
        card.severity === severity &&
        card.category === category &&
        card.blocking === (blocking === "Blocking") &&
        opened.length > 0 &&
        card.fields.length === opened.length &&
        card.fields.every((field, index) => field.label === opened[index].label),
      `reads card ${id} with its card line and every field`,
    );
    for (const [index, { line, label, lead }] of opened.entries()) {
      const { nodes } = card.fields[index];
      const targets = links(line);
      if (lead) {
        const quote = body.slice(body.indexOf(line) + 1).find((next) => next.trim());
        commentLeads++;
        expect(
          nodes.length === 2 &&
            nodes[0].tagName === "p" &&
            nodes[0].children.length === 1 &&
            nodes[0].children[0].tagName === "strong" &&
            drawsLinks(nodes[0], targets) &&
            quote?.startsWith(">") &&
            nodes[1].tagName === "blockquote",
          `draws card ${id}'s ${label} field as its lead line and quote`,
        );
        continue;
      }
      // A card's location, when it has one, is its first field.
      if (index > 0 || !targets.length) continue;
      linkedLocations++;
      expect(drawsLinks(nodes[0], targets), `links each location of card ${id}`);
    }
  }
  expect(linkedLocations > 0, "links a card's location");
  expect(commentLeads > 0, "draws a field's lead line and quote");
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

  // The previous-threads table is the one whose rows open with a finding ID.
  const firstRow = lines.findIndex((line) => /^\| F-\d+ · /.test(line));
  const tableStart = firstRow - 2;
  const tableLength = lines.slice(tableStart).findIndex((line) => !line.startsWith("|"));
  const heading = lines.slice(0, tableStart).findLastIndex((line) => line.startsWith("## "));
  const note = lines.slice(heading + 1, tableStart).some((line) => line.trim());
  const threads = blocks.find((block) => block.name === lines[heading]?.slice(3));
  const nodes = threads?.nodes.filter((node) => node.type === "element") ?? [];
  const table = nodes.find((node) => node.tagName === "table");
  const rows = table && find(table.children, (node) => node.tagName === "tbody")?.children;
  const drawnRows = rows?.filter((node) => node.tagName === "tr") ?? [];
  expect(
    firstRow > 2 &&
      tableLength > 2 &&
      table?.properties.className?.includes("report-previous-threads") &&
      drawnRows.length === tableLength - 2,
    "draws the previous-threads table",
  );
  const drawnNote = nodes.findIndex((node) => node.tagName === "p");
  expect(
    note && drawnNote >= 0 && drawnNote < nodes.indexOf(table),
    "draws the previous-threads note before the table",
  );
  const linkCells = lines.slice(firstRow, tableStart + tableLength).map((line) => {
    const cells = line.split(/(?<!\\)\|/);
    return links(cells.at(-2) ?? "");
  });
  expect(
    linkCells.some((targets) => targets.length) &&
      drawnRows.every((row, index) =>
        drawsLinks(row.children.filter((cell) => cell.tagName === "td").at(-1), linkCells[index]),
      ),
    "links each row's thread and checked revision",
  );
}

// The link targets in composed Markdown, in order, outside code spans. A link's text is
// escaped, so `](` that is not preceded by a backslash ends it.
function links(markdown) {
  const prose = markdown.replace(/(?<!\\)(`+)[\s\S]*?\1/g, "");
  return [...prose.matchAll(/(?<!\\)\]\(([^)\s]+)\)/g)].map(([, target]) => new URL(target).href);
}

function drawsLinks(node, targets) {
  const drawn = [];
  const visit = (child) => {
    if (child.tagName === "a") drawn.push(child.properties.href);
    child.children?.forEach(visit);
  };
  if (node) visit(node);
  return drawn.length === targets.length && drawn.every((href, index) => href === targets[index]);
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
