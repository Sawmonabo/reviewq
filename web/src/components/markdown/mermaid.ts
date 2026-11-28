import type { Element } from "hast";
import type Mermaid from "mermaid";

let engine: Promise<typeof Mermaid> | undefined;

// The source a code block hands Mermaid, or undefined when the block is not a Mermaid
// diagram: the text of a `pre` whose code has the `language-mermaid` class. The syntax tree
// drops the block's last line ending and the HTML step adds "\n", while every other line
// ending stays as written, "\r" included. `diagram_source` in
// crates/reviewq-app/src/reports/drawings.rs writes the same text for the drawings ReviewQ
// makes ahead, which this page finds by its SHA-256.
export function diagramSource(node: Element | undefined) {
  const child = node?.children[0];
  if (
    child?.type !== "element" ||
    child.tagName !== "code" ||
    !Array.isArray(child.properties.className) ||
    !child.properties.className.includes("language-mermaid")
  )
    return;
  return child.children.map((item) => (item.type === "text" ? item.value : "")).join("");
}

export function eligibleDiagram(source: string) {
  return (
    source.length <= 30_000 &&
    (source.match(/-->|---|==>|-.->|->>/g) ?? []).length <= 500 &&
    !/^\s*(?:%%\{|---)/.test(source) &&
    !/\bclick\s|https?:|@import|url\s*\(/i.test(source)
  );
}

export async function renderDiagram(id: string, source: string) {
  engine ??= import("mermaid").then(({ default: mermaid }) => {
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      htmlLabels: false,
      suppressErrorRendering: true,
      maxTextSize: 30_000,
      maxEdges: 500,
      theme: "base",
      look: "classic",
      themeVariables: {
        darkMode: true,
        background: "#0b0c10",
        primaryColor: "#1c2330",
        primaryTextColor: "#e0e5ef",
        primaryBorderColor: "#657894",
        lineColor: "#8c9db7",
        secondaryColor: "#23222d",
        tertiaryColor: "#141820",
        noteBkgColor: "#242735",
        noteTextColor: "#e0e5ef",
        noteBorderColor: "#657894",
        fontFamily: "Inter, Segoe UI, sans-serif",
        fontSize: "14px",
      },
    });
    return mermaid;
  });
  return (await (await engine).render(id, source)).svg;
}

export function parseDiagram(svg: string) {
  const parsed = new DOMParser().parseFromString(svg, "image/svg+xml");
  const root = parsed.documentElement;
  if (!(root instanceof SVGSVGElement)) throw new Error("Invalid diagram");
  const { width, height } = root.viewBox.baseVal;
  if (!Number.isFinite(width) || width <= 0 || !Number.isFinite(height) || height <= 0)
    throw new Error("Invalid diagram size");
  for (const unsafe of parsed.querySelectorAll("script, foreignObject, image, a")) unsafe.remove();
  return { root, size: { width, height } };
}
