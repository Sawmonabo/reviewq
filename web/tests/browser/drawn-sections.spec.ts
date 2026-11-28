import { readFileSync } from "node:fs";
import { expect, test } from "@playwright/test";
import type { Element, Root, RootContent } from "hast";
import { diagramSource, eligibleDiagram } from "../../src/components/markdown/mermaid";
import { parseMarkdown } from "../../src/components/markdown/parse-markdown";
import { digestDrawing } from "../../src/components/markdown/saved-diagram";
import { reportMarkdown } from "../../src/features/review/report/report-reader";
import { reportStructure, reportText } from "../../src/features/review/report/report-structure";

type Body = string | readonly (string | { readonly repeat: string; readonly times: number })[];
type Fixture = {
  readonly headings: readonly {
    readonly case: string;
    readonly cell: string;
    readonly shown: string;
  }[];
  readonly sections: readonly {
    readonly case: string;
    readonly section: "flow" | "changes";
    readonly body: Body;
    readonly drawn: boolean;
    readonly diagrams?: readonly Body[];
    readonly digests?: readonly string[];
  }[];
};

// The cases ReviewQ's save-time checks read too (drawn_sections_agree_with_the_page in
// crates/reviewq-app/src/ledger/markdown/tests.rs), judged here by the page's own code.
const fixture = JSON.parse(
  readFileSync(new URL("../../../fixtures/markdown/drawn-sections.json", import.meta.url), "utf8"),
) as Fixture;
const operations = ["affected operation", "what changes", "source"];

function text(body: Body) {
  return typeof body === "string"
    ? body
    : body
        .map((piece) => (typeof piece === "string" ? piece : piece.repeat.repeat(piece.times)))
        .join("");
}

function report(source: string): Root {
  const tree = parseMarkdown(source, undefined, reportMarkdown);
  if (!tree) throw new Error("The page did not format the section.");
  return tree;
}

function elements(nodes: readonly RootContent[]): Element[] {
  return nodes.flatMap((node) =>
    node.type === "element" ? [node, ...elements(node.children)] : [],
  );
}

// Each header cell of a top-level table, as reportTable compares it.
function headings(table: Element) {
  return elements(table.children)
    .filter((cell) => cell.tagName === "th")
    .map((cell) => reportText(cell).trim().toLowerCase());
}

function isCards(node: RootContent) {
  const name = node.type === "element" ? node.properties.className : undefined;
  return Array.isArray(name) && name.includes("report-before-after");
}

test("the page draws exactly the report sections ReviewQ accepts", async () => {
  for (const { case: name, cell, shown } of fixture.headings) {
    const source = `| ${cell} | After |\n| --- | --- |\n| Reads input unchecked. | Checks it first. |\n`;
    const [table] = reportStructure(report(source).children, source);
    expect(table?.type === "element" && headings(table)[0], name).toBe(shown);
    expect(table && isCards(table), name).toBe(shown === "before");
  }
  for (const { case: name, section, body, drawn, diagrams, digests } of fixture.sections) {
    const source = text(body);
    const tree = report(source);
    if (section === "flow") {
      const drawable = elements(tree.children)
        .map((node) => (node.tagName === "pre" ? diagramSource(node) : undefined))
        .filter((diagram): diagram is string => diagram !== undefined && eligibleDiagram(diagram));
      expect(drawable, name).toEqual((diagrams ?? []).map(text));
      expect(drawable.length > 0, name).toBe(drawn);
      if (digests) {
        // The key the page looks up a drawing made ahead by.
        expect(await Promise.all(drawable.map(digestDrawing)), name).toEqual(digests);
      }
    } else {
      // The page draws the operations table as a plain table, so what must agree is the
      // text of its headings after the first Before and After table.
      const tables = reportStructure(tree.children, source);
      const cards = tables.findIndex(isCards);
      const listed = tables
        .slice(cards + 1)
        .some(
          (node) =>
            node.type === "element" &&
            node.tagName === "table" &&
            headings(node).join("\n") === operations.join("\n"),
        );
      expect(cards >= 0 && listed, name).toBe(drawn);
    }
  }
});
