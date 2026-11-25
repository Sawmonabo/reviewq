import type { Element, Root, RootContent } from "hast";
import rehypeRaw from "rehype-raw";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import remarkRehype from "remark-rehype";
import { unified } from "unified";
import { safeUrl } from "../../lib/presentation";

type MarkdownNode = {
  readonly type: string;
  readonly value?: string | undefined;
  readonly alt?: string | null | undefined;
  readonly children?: readonly MarkdownNode[] | undefined;
};

export type PreparedMarkdown = { readonly tree: Root; readonly nodes: number };
export type MarkdownOptions = {
  // Shows HTML-looking text as written, such as `Vec<u8>` or `<runner>`, instead of
  // reading it as markup. Each `<br>` stays a line break. Report text uses it.
  readonly literalHtml?: boolean;
};
type SyntaxTree = ReturnType<typeof syntax.parse>;
type SyntaxNode = SyntaxTree | SyntaxTree["children"][number];
type SyntaxPosition = NonNullable<SyntaxTree["position"]>;

const MAX_DEPTH = 128;
export const MAX_MARKDOWN_NODES = 60_000;
const MARKDOWN_SYNTAX = /[*_`~[<>&\\|#]|^(?:[ \t]*[+-]|\d+[.)])[ \t]/m;
const blockNodes = new Set(["root", "list", "listItem", "blockquote", "table"]);
// Parents whose children are blocks, so literal HTML there becomes its own paragraph.
const flowParents = new Set(["root", "blockquote", "listItem", "footnoteDefinition"]);
const lineBreak = /<br ?\/?>/g;
const sanitizeSchema = {
  ...defaultSchema,
  tagNames: [...(defaultSchema.tagNames ?? []), "details", "summary"],
  attributes: { ...defaultSchema.attributes, details: ["open"] },
  protocols: { ...defaultSchema.protocols, href: ["https", "http", "mailto"] },
};
const syntax = unified().use(remarkParse).use(remarkGfm);
const rich = syntax()
  .use(remarkRehype, { allowDangerousHtml: true })
  .use(rehypeRaw)
  .use(rehypeSanitize, sanitizeSchema);
const literal = syntax().use(literalHtml).use(remarkRehype).use(rehypeSanitize, sanitizeSchema);

export function parseMarkdown(
  source: string,
  base?: string,
  options?: MarkdownOptions,
): Root | undefined {
  return prepareMarkdown(source, base, options)?.tree;
}

export function prepareMarkdown(
  source: string,
  base?: string,
  options?: MarkdownOptions,
): PreparedMarkdown | undefined {
  if (!admitMarkdown(source)) return;
  const processor = options?.literalHtml ? literal : rich;
  const tree = processor.runSync(processor.parse(source), source);
  const pending: { node: Root | RootContent; depth: number }[] = [{ node: tree, depth: 0 }];
  let count = 0;
  while (pending.length) {
    const entry = pending.pop();
    if (!entry) break;
    if (++count > MAX_MARKDOWN_NODES || entry.depth > MAX_DEPTH) return;
    if (entry.node.type === "element") safeElement(entry.node, base);
    if ("children" in entry.node) {
      for (const node of entry.node.children) pending.push({ node, depth: entry.depth + 1 });
    }
  }
  return { tree, nodes: count };
}

export function projectMarkdownText(source: string): string {
  if (!admitMarkdown(source) || !MARKDOWN_SYNTAX.test(source)) return source;
  // Use the syntax tree for searchable prose without creating the rendered HTML tree.
  try {
    return textContent(syntax.parse(source), 0, { remaining: MAX_MARKDOWN_NODES });
  } catch (error) {
    if (!(error instanceof RangeError)) throw error;
    return source;
  }
}

export function admitMarkdown(source: string): boolean {
  const encoder = new TextEncoder();
  if (source.length > 1024 * 1024 || encoder.encode(source).length > 1024 * 1024) return false;
  const lines = source.split("\n");
  return (
    lines.length - (lines.at(-1) === "" ? 1 : 0) <= 10_000 &&
    lines.every(
      (line) => line.length <= 16_384 && encoder.encode(line.replace(/\r$/, "")).length <= 16_384,
    )
  );
}

function textContent(node: MarkdownNode, depth: number, budget: { remaining: number }): string {
  if (depth > MAX_DEPTH || --budget.remaining < 0)
    throw new RangeError("Markdown formatting limit");
  switch (node.type) {
    case "definition":
      return "";
    case "image":
    case "imageReference":
      return imageLabel(node.alt);
    case "break":
      return "\n";
    case "html": {
      // Template contents remain inert and never enter the page.
      const template = document.createElement("template");
      template.innerHTML = node.value ?? "";
      for (const hidden of template.content.querySelectorAll("script, style, template"))
        hidden.remove();
      for (const image of template.content.querySelectorAll("img"))
        image.replaceWith(imageLabel(image.getAttribute("alt")));
      return template.content.textContent ?? "";
    }
    default: {
      const separator = node.type === "tableRow" ? "\t" : blockNodes.has(node.type) ? "\n" : "";
      return (
        node.value ??
        node.children
          ?.map((child) => textContent(child, depth + 1, budget))
          .filter(Boolean)
          .join(separator) ??
        ""
      );
    }
  }
}

// Turns each HTML node into text with the same value before it reaches HTML, with each
// `<br>`, `<br/>` or `<br />` in it as a line break.
function literalHtml() {
  return (tree: SyntaxTree, file: { readonly value: unknown }) => {
    const source = String(file.value);
    const pending: SyntaxNode[] = [tree];
    for (let node = pending.pop(); node; node = pending.pop()) {
      if (!("children" in node)) continue;
      const flow = flowParents.has(node.type);
      // Each replacement is valid where the HTML node stood: phrasing, or a paragraph in flow.
      const replaced: unknown[] = [];
      for (const child of node.children) {
        if (child.type !== "html") {
          pending.push(child);
          replaced.push(child);
          continue;
        }
        const inline = literalPieces(child.value, child.position, source);
        if (flow) replaced.push({ type: "paragraph", children: inline, position: child.position });
        else replaced.push(...inline);
      }
      const children: unknown[] = node.children;
      children.splice(0, children.length, ...replaced);
    }
  };
}

// Text and line breaks for one HTML node. Pieces carry their own source positions when the
// node's value is its exact source text.
function literalPieces(value: string, position: SyntaxPosition | undefined, source: string) {
  const breaks = [...value.matchAll(lineBreak)];
  if (!breaks.length) return [{ type: "text", value, position }];
  const start = position?.start.offset;
  const exact =
    position && start !== undefined && source.slice(start, position.end.offset) === value
      ? position
      : undefined;
  const span = (from: number, to: number) =>
    exact && { position: { start: pointAt(exact, value, from), end: pointAt(exact, value, to) } };
  const pieces: unknown[] = [];
  let at = 0;
  for (const found of breaks) {
    if (found.index > at)
      pieces.push({ type: "text", value: value.slice(at, found.index), ...span(at, found.index) });
    at = found.index + found[0].length;
    pieces.push({ type: "break", ...span(found.index, at) });
  }
  if (at < value.length)
    pieces.push({ type: "text", value: value.slice(at), ...span(at, value.length) });
  return pieces;
}

function pointAt(position: SyntaxPosition, value: string, at: number) {
  const before = value.slice(0, at);
  const lastLine = before.lastIndexOf("\n");
  return {
    line: position.start.line + before.split("\n").length - 1,
    column: lastLine < 0 ? position.start.column + at : at - lastLine,
    offset: (position.start.offset ?? 0) + at,
  };
}

function imageLabel(alt: unknown): string {
  return typeof alt === "string" && alt ? alt : "Attachment";
}

function safeElement(node: Element, base: string | undefined) {
  if (node.tagName === "img") {
    const href =
      typeof node.properties.src === "string" ? safeUrl(node.properties.src, base) : undefined;
    const label = imageLabel(node.properties.alt);
    node.tagName = href ? "a" : "span";
    node.properties = href ? { href, target: "_blank", rel: ["noreferrer"] } : {};
    node.children = [{ type: "text", value: label }];
  } else if (node.tagName === "a" && typeof node.properties.href === "string") {
    if (node.properties.href.startsWith("#")) return;
    const href = safeUrl(node.properties.href, base);
    if (href) node.properties = { ...node.properties, href, target: "_blank", rel: ["noreferrer"] };
    else delete node.properties.href;
  }
}
