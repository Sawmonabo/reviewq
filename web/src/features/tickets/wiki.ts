import remarkParse from "remark-parse";
import { unified } from "unified";

const markdown = unified().use(remarkParse);
const wikiMacro = /\{(code|noformat)(?::([^}\r\n]*))?\}([\s\S]*?)\{\1\}/g;
const wikiProseMarker = /(?:^h[1-6]\.[ \t]|^\|\||\{\{[^}\r\n]+\}\}|\[[^\]\n]+\|https?:)/m;

type SourceRange = { start: number; end: number; replacement?: string };
type MarkdownNode = {
  readonly type: string;
  readonly children?: readonly MarkdownNode[];
  readonly position?:
    | {
        readonly start: { readonly offset?: number | undefined };
        readonly end: { readonly offset?: number | undefined };
      }
    | undefined;
};

function codeRanges(node: MarkdownNode, ranges: SourceRange[]) {
  if (node.type === "code" || node.type === "inlineCode") {
    const start = node.position?.start.offset;
    const end = node.position?.end.offset;
    if (start !== undefined && end !== undefined) ranges.push({ start, end });
    return;
  }
  for (const child of node.children ?? []) codeRanges(child, ranges);
}

// Apply Jira's v2 wiki conversions to prose while preserving original Markdown code.
export function jiraWiki(source: string): string {
  if (
    !/(?:^h[1-6]\.[ \t]|^[*#]+[ \t]|\{(?:code|noformat)\b|\{\{|^\|\||\[[^\]\n]+\|https?:)/m.test(
      source,
    )
  )
    return source;
  const ranges: SourceRange[] = [];
  codeRanges(markdown.parse(source), ranges);
  for (const match of source.matchAll(wikiMacro)) {
    ranges.push({
      start: match.index,
      end: match.index + match[0].length,
      replacement: wikiCode(match[3] ?? "", match[2]),
    });
  }
  let position = 0;
  const chunks: { text: string; kind: "prose" | "code" | "wiki-code" }[] = [];
  for (const range of ranges.sort((a, b) => a.start - b.start)) {
    if (range.start < position) continue;
    chunks.push({ text: source.slice(position, range.start), kind: "prose" });
    chunks.push({
      text: range.replacement ?? source.slice(range.start, range.end),
      kind: range.replacement === undefined ? "code" : "wiki-code",
    });
    position = range.end;
  }
  chunks.push({ text: source.slice(position), kind: "prose" });
  const jiraLists = chunks.some(
    ({ kind, text }) => kind === "wiki-code" || (kind === "prose" && wikiProseMarker.test(text)),
  );
  return chunks
    .map(({ kind, text }) => (kind === "prose" ? wikiProse(text, jiraLists) : text))
    .join("");
}

function wikiCode(body: string, options: string | undefined) {
  let length = 3;
  for (const [run] of body.matchAll(/`+/g)) length = Math.max(length, run.length + 1);
  const fence = "`".repeat(length);
  const language = options?.split("|", 1)[0] ?? "";
  const text = body.replace(/^\r?\n|\r?\n$/g, "");
  return `\n\n${fence}${/^[\w+-]+$/.test(language) ? language : ""}\n${text}\n${fence}\n\n`;
}

function wikiProse(source: string, jiraLists: boolean) {
  return source
    .replace(/^([*#]+)[ \t]+/gm, (match: string, prefix: string) => {
      if (!jiraLists && !prefix.includes("*")) return match;
      const indent = [...prefix.slice(0, -1)].map((type) => (type === "#" ? "   " : "  ")).join("");
      return `${indent}${prefix.endsWith("#") ? "1." : "-"} `;
    })
    .replace(/^h([1-6])\.[ \t]+/gm, (_match, level: string) => `${"#".repeat(Number(level))} `)
    .replace(/\[([^|\]\r\n]+)\|(https?:[^\]\r\n]+)\]/g, "[$1]($2)")
    .replace(/\{\{([^}\r\n]+)\}\}/g, "`$1`")
    .replace(/^\|\|([^\r\n]*)\|\|$/gm, (_match, cells: string) => {
      const values = cells.split("||");
      return `| ${values.join(" | ")} |\n| ${values.map(() => "---").join(" | ")} |`;
    });
}
