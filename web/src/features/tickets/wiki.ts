import remarkParse from "remark-parse";
import { unified } from "unified";

const markdown = unified().use(remarkParse);
const wikiMacro = /\{(code|noformat)(?::([^}\r\n]*))?\}([\s\S]*?)\{\1\}/g;

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
  let converted = "";
  for (const range of ranges.sort((a, b) => a.start - b.start)) {
    if (range.start < position) continue;
    converted += wikiProse(source.slice(position, range.start));
    converted += range.replacement ?? source.slice(range.start, range.end);
    position = range.end;
  }
  return converted + wikiProse(source.slice(position));
}

function wikiCode(body: string, options: string | undefined) {
  let length = 3;
  for (const [run] of body.matchAll(/`+/g)) length = Math.max(length, run.length + 1);
  const fence = "`".repeat(length);
  const language = options?.split("|", 1)[0] ?? "";
  const text = body.replace(/^\r?\n|\r?\n$/g, "");
  return `\n\n${fence}${/^[\w+-]+$/.test(language) ? language : ""}\n${text}\n${fence}\n\n`;
}

function wikiProse(source: string) {
  return source
    .replace(/^h([1-6])\.[ \t]+/gm, (_match, level: string) => `${"#".repeat(Number(level))} `)
    .replace(
      /^([*#]+)[ \t]+/gm,
      (_match, prefix: string) =>
        `${"  ".repeat(prefix.length - 1)}${prefix.endsWith("#") ? "1." : "-"} `,
    )
    .replace(/\[([^|\]\r\n]+)\|(https?:[^\]\r\n]+)\]/g, "[$1]($2)")
    .replace(/\{\{([^}\r\n]+)\}\}/g, "`$1`")
    .replace(/^\|\|([^\r\n]*)\|\|$/gm, (_match, cells: string) => {
      const values = cells.split("||");
      return `| ${values.join(" | ")} |\n| ${values.map(() => "---").join(" | ")} |`;
    });
}
