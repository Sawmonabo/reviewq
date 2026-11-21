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

const MAX_DEPTH = 128;
const MAX_NODES = 60_000;
const MARKDOWN_SYNTAX = /[*_`~[<>&\\|#]|^(?:[ \t]*[+-]|\d+[.)])[ \t]/m;
const blockNodes = new Set(["root", "list", "listItem", "blockquote", "table"]);
const syntax = unified().use(remarkParse).use(remarkGfm);
const rich = syntax()
  .use(remarkRehype, { allowDangerousHtml: true })
  .use(rehypeRaw)
  .use(rehypeSanitize, {
    ...defaultSchema,
    tagNames: [...(defaultSchema.tagNames ?? []), "details", "summary"],
    attributes: { ...defaultSchema.attributes, details: ["open"] },
    protocols: { ...defaultSchema.protocols, href: ["https", "http", "mailto"] },
  });

export function parseMarkdown(source: string, base?: string): Root | undefined {
  if (!admitMarkdown(source)) return;
  const tree = rich.runSync(rich.parse(source));
  const pending: { node: Root | RootContent; depth: number }[] = [{ node: tree, depth: 0 }];
  let count = 0;
  while (pending.length) {
    const entry = pending.pop();
    if (!entry) break;
    if (++count > MAX_NODES || entry.depth > MAX_DEPTH) return;
    if (entry.node.type === "element") safeElement(entry.node, base);
    if ("children" in entry.node) {
      for (const node of entry.node.children) pending.push({ node, depth: entry.depth + 1 });
    }
  }
  return tree;
}

export function projectMarkdownText(source: string): string {
  if (!admitMarkdown(source) || !MARKDOWN_SYNTAX.test(source)) return source;
  // Use the syntax tree for searchable prose without creating the rendered HTML tree.
  try {
    return textContent(syntax.parse(source), 0, { remaining: MAX_NODES });
  } catch (error) {
    if (!(error instanceof RangeError)) throw error;
    return source;
  }
}

function admitMarkdown(source: string): boolean {
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
