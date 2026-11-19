import { useMemo } from "react";
import type { ReactNode } from "react";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import type { Element, Root } from "hast";
import MermaidDiagram from "./MermaidDiagram";
import { parseMarkdown } from "./parse-markdown";
import "./markdown.css";

export function MarkdownContent({
  source,
  base,
}: {
  readonly source: string;
  readonly base?: string | undefined;
}) {
  const tree = useMemo(() => parseMarkdown(source, base), [source, base]);
  return tree ? (
    <MarkdownTree tree={tree} />
  ) : (
    <details className="surface-card">
      <summary>Markdown source · formatting limit</summary>
      <pre className="prose-source">{source.slice(0, 65_536)}</pre>
      {source.length > 65_536 && <p className="meta">Source preview ends here.</p>}
    </details>
  );
}

export function MarkdownTree({ tree }: { readonly tree: Root }) {
  return (
    <div className="prose">
      {toJsxRuntime(tree, {
        Fragment,
        jsx,
        jsxs,
        passNode: true,
        components: {
          pre: CodeBlock,
          table: ({ children, className }) => (
            <div className="table-scroll" tabIndex={0}>
              <table className={className}>{children}</table>
            </div>
          ),
        },
      })}
    </div>
  );
}

function CodeBlock({
  node,
  children,
}: {
  readonly node?: Element | undefined;
  readonly children?: ReactNode;
}) {
  const child = node?.children[0];
  if (
    child?.type === "element" &&
    child.tagName === "code" &&
    Array.isArray(child.properties.className) &&
    child.properties.className.includes("language-mermaid")
  ) {
    const source = child.children.map((item) => (item.type === "text" ? item.value : "")).join("");
    return <MermaidDiagram source={source} />;
  }
  return <pre>{children}</pre>;
}
