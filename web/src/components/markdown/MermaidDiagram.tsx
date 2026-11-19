import type Mermaid from "mermaid";
import { useContext, useEffect, useId, useState } from "react";
import { DiagramCacheContext, reusableDiagram } from "./diagram-cache";
import { useDiagramViewport } from "./useDiagramViewport";

let engine: Promise<typeof Mermaid> | undefined;

export default function MermaidDiagram({ source }: { readonly source: string }) {
  const cache = useContext(DiagramCacheContext);
  const id = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const [result, setResult] = useState<{
    source: string;
    size: { width: number; height: number } | null;
  }>();
  const size = result?.source === source ? result.size : undefined;
  const { frame, canvas, zoom, minimum, zoomIn, zoomOut, reset, fit, gestures } =
    useDiagramViewport(size ?? undefined);
  const eligible =
    source.length <= 30_000 &&
    (source.match(/-->|---|==>|-.->|->>/g) ?? []).length <= 500 &&
    !/^\s*(?:%%\{|---)/.test(source) &&
    !/\bclick\s|https?:|@import|url\s*\(/i.test(source);
  useEffect(() => {
    const host = frame.current;
    const target = canvas.current;
    if (!eligible || !host || !target) return;
    let active = true;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      observer.disconnect();
      const theme = document.documentElement.dataset.theme ?? "";
      const renderId = `reviewq-diagram-${crypto.randomUUID()}`;
      const saved = cache?.diagram;
      const hit = saved?.source === source && saved.theme === theme ? saved : undefined;
      const render = async () => {
        if (hit) return hit.svg.replaceAll(hit.id, renderId);
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
        return (await (await engine).render(renderId, source)).svg;
      };
      void render()
        .then((svg) => {
          if (!active) return;
          const parsed = new DOMParser().parseFromString(svg, "image/svg+xml");
          const root = parsed.documentElement;
          if (!(root instanceof SVGSVGElement)) throw new Error("Invalid diagram");
          const { width, height } = root.viewBox.baseVal;
          if (!Number.isFinite(width) || width <= 0 || !Number.isFinite(height) || height <= 0)
            throw new Error("Invalid diagram size");
          for (const unsafe of parsed.querySelectorAll("script, foreignObject, image, a"))
            unsafe.remove();
          if (cache && !hit) {
            const reusable = reusableDiagram(root, renderId, svg.length);
            if (reusable) cache.diagram = { source, theme, id: renderId, svg: reusable };
          }
          target.replaceChildren(document.importNode(root, true));
          setResult({ source, size: { width, height } });
        })
        .catch(() => {
          if (active) setResult({ source, size: null });
        });
    });
    observer.observe(host);
    return () => {
      active = false;
      observer.disconnect();
      target.replaceChildren();
    };
  }, [eligible, source, frame, canvas, cache]);

  const idle = eligible && size === undefined;
  const failed = !eligible || size === null;
  return (
    <figure className="mermaid-figure" aria-labelledby={`diagram-caption-${id}`}>
      <figcaption id={`diagram-caption-${id}`}>
        <span>
          Diagram<small className="diagram-hint">Drag to pan · Scroll to zoom</small>
        </span>
        {size && (
          <div className="diagram-controls" aria-label="Diagram zoom">
            <button
              type="button"
              disabled={zoom <= minimum}
              onClick={zoomOut}
              aria-label="Zoom out diagram"
            >
              −
            </button>
            <button type="button" onClick={reset} aria-label="Reset diagram zoom">
              {Math.round(zoom * 100)}%
            </button>
            <button
              type="button"
              disabled={zoom >= 3}
              onClick={zoomIn}
              aria-label="Zoom in diagram"
            >
              +
            </button>
            <button type="button" onClick={fit} aria-label="Fit diagram to panel">
              Fit
            </button>
          </div>
        )}
      </figcaption>
      {idle && (
        <p className="diagram-loading" role="status">
          Rendering diagram…
        </p>
      )}
      {failed && (
        <p className="diagram-loading" role="status">
          This diagram could not be rendered. Its source is available below.
        </p>
      )}
      {eligible && (
        <div
          ref={frame}
          className="diagram-frame"
          tabIndex={size ? 0 : -1}
          aria-label="Mermaid diagram"
          {...gestures}
        >
          <div ref={canvas} className="diagram-art" hidden={!size} />
        </div>
      )}
      <details className="diagram-source" open={failed}>
        <summary>Diagram source</summary>
        <pre>{source}</pre>
      </details>
    </figure>
  );
}
