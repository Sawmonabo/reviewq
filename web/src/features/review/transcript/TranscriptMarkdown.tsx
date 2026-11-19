import { lazy, Suspense } from "react";

const Markdown = lazy(() =>
  import("../../../components/markdown/MarkdownContent").then((module) => ({
    default: module.MarkdownContent,
  })),
);

export function TranscriptMarkdown({ source }: { readonly source: string }) {
  return (
    <Suspense fallback={<p className="message-copy">{source}</p>}>
      <Markdown source={source} />
    </Suspense>
  );
}
