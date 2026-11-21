import { createContext, useContext, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import type { Element, Root } from "hast";
import type { DocumentDisplay } from "../../api/generated/contracts/common";
import { DiagramCacheContext, DiagramDisplayContext } from "./diagram-cache";
import { prepareLiveMarkdown, type LiveMarkdown } from "./live/blocks";
import MermaidDiagram from "./MermaidDiagram";
import { parseMarkdown } from "./parse-markdown";
import "./markdown.css";

type MarkdownContentProps = { readonly display?: DocumentDisplay | null | undefined } & (
  | { readonly tree: Root }
  | {
      readonly source: string;
      readonly base?: string | undefined;
      readonly provisional?: boolean;
      readonly formattingLimit?: boolean;
    }
);

const ProvisionalMarkdown = createContext(false);

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
  return tree ? (
    <DiagramDisplayContext value={provisional ? undefined : props.display?.drawings}>
      <MarkdownTree tree={tree} provisional={provisional ?? false} />
    </DiagramDisplayContext>
  ) : (
    <MarkdownSource source={source ?? ""} />
  );
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

export function MarkdownTree({
  tree,
  provisional = false,
}: {
  readonly tree: Root;
  readonly provisional?: boolean;
}) {
  return (
    <div className="prose">
      <ProvisionalMarkdown value={provisional}>
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
      </ProvisionalMarkdown>
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
  const provisional = useContext(ProvisionalMarkdown);
  const scope = useContext(DiagramCacheContext)?.scope ?? "";
  const child = node?.children[0];
  if (
    child?.type === "element" &&
    child.tagName === "code" &&
    Array.isArray(child.properties.className) &&
    child.properties.className.includes("language-mermaid")
  ) {
    const source = child.children.map((item) => (item.type === "text" ? item.value : "")).join("");
    return (
      <MermaidDiagram
        key={JSON.stringify([scope, node?.position?.start.offset])}
        source={source}
        provisional={provisional}
      />
    );
  }
  return <pre>{children}</pre>;
}
