import type { Element, Root, RootContent } from "hast";
import rehypeRaw from "rehype-raw";
import rehypeSanitize, { defaultSchema } from "rehype-sanitize";
import remarkGfm from "remark-gfm";
import remarkParse from "remark-parse";
import remarkRehype from "remark-rehype";
import { unified } from "unified";
import { safeUrl } from "../../lib/presentation";

const sentence = unified()
  .use(remarkParse)
  .use(remarkGfm)
  .use(remarkRehype, { allowDangerousHtml: true })
  .use(rehypeRaw)
  .use(rehypeSanitize, {
    ...defaultSchema,
    tagNames: [...(defaultSchema.tagNames ?? []), "details", "summary"],
    attributes: { ...defaultSchema.attributes, details: ["open"] },
    protocols: { ...defaultSchema.protocols, href: ["https", "http", "mailto"] },
  });

export function parseMarkdown(source: string, base?: string): Root | undefined {
  const encoder = new TextEncoder();
  if (source.length > 1024 * 1024 || encoder.encode(source).length > 1024 * 1024) return;
  const lines = source.split("\n");
  if (
    lines.length - (lines.at(-1) === "" ? 1 : 0) > 10_000 ||
    lines.some(
      (line) => line.length > 16_384 || encoder.encode(line.replace(/\r$/, "")).length > 16_384,
    )
  )
    return;
  const tree = sentence.runSync(sentence.parse(source));
  const pending: { node: Root | RootContent; depth: number }[] = [{ node: tree, depth: 0 }];
  let count = 0;
  while (pending.length) {
    const entry = pending.pop();
    if (!entry) break;
    if (++count > 60_000 || entry.depth > 128) return;
    if (entry.node.type === "element") safeElement(entry.node, base);
    if ("children" in entry.node) {
      for (const node of entry.node.children) pending.push({ node, depth: entry.depth + 1 });
    }
  }
  return tree;
}

function safeElement(node: Element, base: string | undefined) {
  if (node.tagName === "img") {
    const href =
      typeof node.properties.src === "string" ? safeUrl(node.properties.src, base) : undefined;
    const label =
      typeof node.properties.alt === "string" && node.properties.alt
        ? node.properties.alt
        : "Attachment";
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
