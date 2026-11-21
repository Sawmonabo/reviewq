import { useMemo, useState } from "react";
import type { ReactNode } from "react";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import type { Element, Root } from "hast";
import { prepareLiveMarkdown, type LiveMarkdown } from "./live/blocks";
import MermaidDiagram from "./MermaidDiagram";
import { parseMarkdown } from "./parse-markdown";
import "./markdown.css";

type MarkdownContentProps =
  | { readonly tree: Root }
  | {
      readonly source: string;
      readonly base?: string | undefined;
      readonly provisional?: boolean;
      readonly formattingLimit?: boolean;
    };

export function MarkdownContent(props: MarkdownContentProps) {
  const { source, base, provisional, formattingLimit } = "source" in props ? props : {};
  const prepared = "tree" in props ? props.tree : undefined;
  const canonical = useMemo(
    () =>
      source !== undefined && !provisional && !formattingLimit
        ? parseMarkdown(source, base)
        : prepared,
    [source, base, provisional, formattingLimit, prepared],
  );
  const [live, setLive] = useState<LiveMarkdown>();
  let tree = canonical;
  if (provisional && !formattingLimit && source !== undefined) {
    const current =
      live?.source === source && live.base === base
        ? live
        : prepareLiveMarkdown(source, base, live);
    if (current !== live) setLive(current);
    tree = current.tree;
  } else if (live) {
    setLive(undefined);
  }
  return tree ? <MarkdownTree tree={tree} /> : <MarkdownSource source={source ?? ""} />;
}

function MarkdownSource({ source }: { readonly source: string }) {
  return (
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
          table: MarkdownTable,
        },
      })}
    </div>
  );
}

function MarkdownTable({
  children,
  className,
}: {
  readonly children?: ReactNode;
  readonly className?: string;
}) {
  return (
    <div className="table-scroll" tabIndex={0}>
      <table className={className}>{children}</table>
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
