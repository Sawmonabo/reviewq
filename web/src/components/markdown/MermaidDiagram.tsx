import type Mermaid from "mermaid";
import { useEffect, useId, useRef, useState } from "react";

let engine: Promise<typeof Mermaid> | undefined;

export default function MermaidDiagram({ source }: { readonly source: string }) {
  const id = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const frame = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<"idle" | "ready" | "error">("idle");
  const [zoom, setZoom] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const pointer = useRef<{ x: number; y: number } | undefined>(undefined);
  useEffect(() => {
    const host = frame.current;
    const target = canvas.current;
    if (!host || !target) return;
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
          theme: "neutral",
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
  }, [id, source]);

  const eligible =
    source.length <= 30_000 &&
    (source.match(/-->|---|==>|-.->|->>/g) ?? []).length <= 500 &&
    !/^\s*(?:%%\{|---)/.test(source) &&
    !/\bclick\s|https?:|@import|url\s*\(/i.test(source);
  if (!eligible)
    return (
      <details className="surface-card">
        <summary>Diagram source</summary>
        <pre>{source}</pre>
      </details>
    );

  function zoomAt(factor: number, x: number, y: number) {
    const next = Math.max(0.25, Math.min(5, zoom * factor));
    setOffset({ x: x - ((x - offset.x) * next) / zoom, y: y - ((y - offset.y) * next) / zoom });
    setZoom(next);
  }

  return (
    <figure className="mermaid-figure">
      <div className="actions diagram-controls">
        <button
          type="button"
          disabled={state !== "ready"}
          onClick={() => zoomAt(0.8, 0, 0)}
          aria-label="Zoom diagram out"
        >
          −
        </button>
        <button
          type="button"
          disabled={state !== "ready"}
          onClick={() => zoomAt(1.25, 0, 0)}
          aria-label="Zoom diagram in"
        >
          +
        </button>
        <button
          type="button"
          disabled={state !== "ready"}
          onClick={() => {
            setZoom(1);
            setOffset({ x: 0, y: 0 });
          }}
        >
          Fit
        </button>
      </div>
      <div
        ref={frame}
        className="diagram-frame"
        tabIndex={0}
        aria-label="Mermaid diagram"
        onWheel={(event) => {
          if (!event.ctrlKey || state !== "ready") return;
          const bounds = event.currentTarget.getBoundingClientRect();
          zoomAt(
            event.deltaY < 0 ? 1.15 : 0.87,
            event.clientX - bounds.left,
            event.clientY - bounds.top,
          );
        }}
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
        {state === "idle" && <span className="meta">Loading diagram…</span>}
        {state === "error" && <span className="meta">The diagram could not be displayed.</span>}
        <div
          ref={canvas}
          className="diagram-art"
          style={{ transform: `translate(${offset.x}px, ${offset.y}px) scale(${zoom})` }}
        />
      </div>
      <details>
        <summary>Diagram source</summary>
        <pre>{source}</pre>
      </details>
    </figure>
  );
}
