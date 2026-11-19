import type Mermaid from "mermaid";
import { useEffect, useEffectEvent, useId, useRef, useState } from "react";

let engine: Promise<typeof Mermaid> | undefined;

export default function MermaidDiagram({ source }: { readonly source: string }) {
  const id = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const frame = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<"idle" | "ready" | "error">("idle");
  const [zoom, setZoom] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const eligible =
    source.length <= 30_000 &&
    (source.match(/-->|---|==>|-.->|->>/g) ?? []).length <= 500 &&
    !/^\s*(?:%%\{|---)/.test(source) &&
    !/\bclick\s|https?:|@import|url\s*\(/i.test(source);
  const pointer = useRef<{ x: number; y: number } | undefined>(undefined);
  useEffect(() => {
    const host = frame.current;
    const target = canvas.current;
    if (!eligible || !host || !target) return;
    let active = true;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      observer.disconnect();
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
      void engine
        .then((mermaid) => mermaid.render(`reviewq-diagram-${id}`, source))
        .then(({ svg }) => {
          if (!active) return;
          const parsed = new DOMParser().parseFromString(svg, "image/svg+xml");
          if (parsed.documentElement.localName !== "svg") throw new Error("Invalid diagram");
          for (const unsafe of parsed.querySelectorAll("script, foreignObject, image, a"))
            unsafe.remove();
          target.replaceChildren(document.importNode(parsed.documentElement, true));
          setState("ready");
        })
        .catch(() => {
          if (active) setState("error");
        });
    });
    observer.observe(host);
    return () => {
      active = false;
      observer.disconnect();
      target.replaceChildren();
    };
  }, [eligible, id, source]);

  function zoomAt(factor: number, x: number, y: number) {
    const next = Math.max(0.25, Math.min(3, zoom * factor));
    setOffset({ x: x - ((x - offset.x) * next) / zoom, y: y - ((y - offset.y) * next) / zoom });
    setZoom(next);
  }

  const wheelZoom = useEffectEvent((event: WheelEvent) => {
    if (!event.deltaY || state !== "ready" || !(event.currentTarget instanceof HTMLElement)) return;
    event.preventDefault();
    event.stopPropagation();
    const bounds = event.currentTarget.getBoundingClientRect();
    zoomAt(event.deltaY < 0 ? 1.15 : 0.87, event.clientX - bounds.left, event.clientY - bounds.top);
  });
  useEffect(() => {
    const node = frame.current;
    if (!node || state !== "ready") return;
    const wheel = (event: WheelEvent) => wheelZoom(event);
    node.addEventListener("wheel", wheel, { passive: false });
    return () => node.removeEventListener("wheel", wheel);
  }, [state]);

  function fit() {
    setZoom(1);
    setOffset({ x: 0, y: 0 });
  }
  const idle = eligible && state === "idle";
  const failed = !eligible || state === "error";
  return (
    <figure className="mermaid-figure" aria-labelledby={`diagram-caption-${id}`}>
      <figcaption id={`diagram-caption-${id}`}>
        <span>
          Diagram<small className="diagram-hint">Drag to pan · Scroll to zoom</small>
        </span>
        {state === "ready" && (
          <div className="diagram-controls" aria-label="Diagram zoom">
            <button
              type="button"
              disabled={zoom <= 0.25}
              onClick={() => zoomAt(0.8, 0, 0)}
              aria-label="Zoom out diagram"
            >
              −
            </button>
            <button type="button" onClick={fit} aria-label="Reset diagram zoom">
              {Math.round(zoom * 100)}%
            </button>
            <button
              type="button"
              disabled={zoom >= 3}
              onClick={() => zoomAt(1.25, 0, 0)}
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
          tabIndex={state === "ready" ? 0 : -1}
          aria-label="Mermaid diagram"
          onKeyDown={(event) => {
            if (event.key === "+" || event.key === "=") zoomAt(1.25, 0, 0);
            else if (event.key === "-") zoomAt(0.8, 0, 0);
            else if (event.key === "0") {
              setZoom(1);
              setOffset({ x: 0, y: 0 });
            } else return;
            event.preventDefault();
          }}
          onPointerDown={(event) => {
            if (state !== "ready" || event.button !== 0) return;
            pointer.current = { x: event.clientX, y: event.clientY };
            event.currentTarget.setPointerCapture(event.pointerId);
          }}
          onPointerMove={(event) => {
            const last = pointer.current;
            if (!last || !event.currentTarget.hasPointerCapture(event.pointerId)) return;
            setOffset((previous) => ({
              x: previous.x + event.clientX - last.x,
              y: previous.y + event.clientY - last.y,
            }));
            pointer.current = { x: event.clientX, y: event.clientY };
          }}
          onLostPointerCapture={() => {
            pointer.current = undefined;
          }}
        >
          <div
            ref={canvas}
            className="diagram-art"
            style={{ transform: `translate(${offset.x}px, ${offset.y}px) scale(${zoom})` }}
          />
        </div>
      )}
      <details className="diagram-source" open={failed}>
        <summary>Diagram source</summary>
        <pre>{source}</pre>
      </details>
    </figure>
  );
}
