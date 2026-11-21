import { useContext, useEffectEvent, useId, useLayoutEffect, useRef, useState } from "react";
import { completedDiagram } from "./completed-diagram";
import { DiagramCacheContext, DiagramDisplayContext } from "./diagram-cache";
import { eligibleDiagram, parseDiagram, renderDiagram } from "./mermaid";
import { useDiagramViewport } from "./useDiagramViewport";

type Drawing = {
  readonly source: string;
  readonly provisional: boolean;
  readonly svg: SVGSVGElement;
  readonly size: { readonly width: number; readonly height: number };
};

type Revision = {
  readonly source: string;
  readonly provisional: boolean;
  readonly eligible: boolean;
};
type Rendering = {
  inView: boolean;
  closed: boolean;
  running: boolean;
  requested?: Revision;
  attempted?: Revision;
  timer: number | undefined;
};

const LIVE_RENDER_INTERVAL_MS = 48;

export default function MermaidDiagram({
  source,
  provisional,
}: {
  readonly source: string;
  readonly provisional: boolean;
}) {
  const cache = useContext(DiagramCacheContext);
  const display = useContext(DiagramDisplayContext);
  const id = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const [drawing, setDrawing] = useState<Drawing>();
  const [failedSource, setFailedSource] = useState<string>();
  const [sourceOpen, setSourceOpen] = useState<boolean>();
  const figure = useRef<HTMLElement>(null);
  const rendering = useRef<Rendering | undefined>(undefined);
  if (drawing?.source === source && drawing.provisional && !provisional)
    setDrawing({ ...drawing, provisional: false });
  const displayed =
    drawing && (drawing.source === source || provisional || drawing.provisional)
      ? drawing
      : undefined;
  const size = displayed?.size;
  const {
    frame,
    canvas,
    surface,
    extent,
    zoom,
    minimum,
    zoomIn,
    zoomOut,
    reset,
    fit,
    touchPan,
    toggleTouchPan,
    onKeyDown,
    gestures,
  } = useDiagramViewport(size);
  const eligible = eligibleDiagram(source);
  const settled = useEffectEvent(() => drawing?.source === source || failedSource === source);

  const prepare = useEffectEvent(async (state: Rendering, revision: Revision) => {
    const { source, provisional } = revision;
    const renderId = `reviewq-diagram-${crypto.randomUUID()}`;
    try {
      const { root, size } = provisional
        ? parseDiagram(await renderDiagram(renderId, source))
        : await completedDiagram(renderId, source, display, cache);
      const current = state.requested;
      // A completed live draw is useful while this fence is still live. The same
      // serial renderer will prepare the newest queued source on its next turn.
      if (state.closed || (current !== revision && !(current?.provisional && provisional))) return;
      setDrawing({ source, provisional, size, svg: document.importNode(root, true) });
      setFailedSource(undefined);
    } catch {
      if (!state.closed && state.requested === revision) setFailedSource(source);
    }
  });

  const schedule = useEffectEvent(function schedule(state: Rendering) {
    if (
      state.closed ||
      !state.inView ||
      state.running ||
      state.timer !== undefined ||
      !state.requested?.eligible ||
      state.requested === state.attempted
    )
      return;
    state.timer = window.setTimeout(
      () => {
        state.timer = undefined;
        const revision = state.requested;
        if (state.closed || !state.inView || !revision?.eligible || revision === state.attempted)
          return;
        state.attempted = revision;
        state.running = true;
        void prepare(state, revision).finally(() => {
          state.running = false;
          schedule(state);
        });
      },
      state.requested.provisional ? LIVE_RENDER_INTERVAL_MS : 0,
    );
  });

  useLayoutEffect(() => {
    const host = figure.current;
    if (!host) return;
    const state: Rendering = { inView: false, closed: false, running: false, timer: undefined };
    rendering.current = state;
    const observer = new IntersectionObserver(
      (entries) => {
        state.inView = entries.some((entry) => entry.isIntersecting);
        schedule(state);
      },
      { scrollMargin: "120px 0px" },
    );
    observer.observe(host);
    return () => {
      state.closed = true;
      rendering.current = undefined;
      observer.disconnect();
      window.clearTimeout(state.timer);
    };
  }, []);

  useLayoutEffect(() => {
    const state = rendering.current;
    if (!state) return;
    const revision = { source, provisional, eligible };
    state.requested = revision;
    if (settled()) state.attempted = revision;
    schedule(state);
  }, [source, provisional, eligible]);

  useLayoutEffect(() => {
    // Swap the prepared drawing and its viewport dimensions in the same commit.
    const target = canvas.current;
    if (target && target.firstChild !== (displayed?.svg ?? null)) {
      if (displayed) target.replaceChildren(displayed.svg);
      else target.replaceChildren();
    }
  }, [canvas, displayed, eligible]);

  const failed = !eligible || failedSource === source;
  const idle = eligible && !displayed && !failed;
  const lastPreview = displayed && displayed.source !== source && failed && !provisional;
  const openSource =
    sourceOpen ?? ((!displayed && (failed || provisional)) || Boolean(lastPreview));
  return (
    <figure ref={figure} className="mermaid-figure" aria-labelledby={`diagram-caption-${id}`}>
      <figcaption id={`diagram-caption-${id}`}>
        <span>
          Diagram<small className="diagram-hint">Drag to pan · Ctrl/⌘ + scroll to zoom</small>
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
            <button
              type="button"
              className="diagram-touch-pan"
              aria-label="Pan diagram"
              aria-pressed={touchPan}
              onClick={toggleTouchPan}
            >
              Pan
            </button>
          </div>
        )}
      </figcaption>
      {idle && (
        <p className="diagram-loading" role="status">
          Rendering diagram…
        </p>
      )}
      {failed && !displayed && (
        <p className="diagram-loading" role="status">
          This diagram could not be rendered. Its source is available below.
        </p>
      )}
      {(eligible || size) && (
        <div className="diagram-viewport">
          <div
            className="diagram-gesture-surface"
            ref={surface}
            hidden={!size}
            data-touch-pan={touchPan || undefined}
            {...gestures}
          >
            <div ref={canvas} className="diagram-art" />
          </div>
          <div
            ref={frame}
            className="diagram-frame"
            tabIndex={size ? 0 : -1}
            aria-label="Mermaid diagram"
            onKeyDown={onKeyDown}
          >
            <div ref={extent} className="diagram-extent" hidden={!size} aria-hidden="true" />
          </div>
        </div>
      )}
      {lastPreview && (
        <p className="diagram-loading" role="status">
          The final source could not be rendered. The drawing is the last valid preview.
        </p>
      )}
      <details className="diagram-source" open={openSource}>
        <summary
          onClick={(event) => {
            event.preventDefault();
            setSourceOpen(!openSource);
          }}
        >
          Diagram source
        </summary>
        <pre>{source}</pre>
      </details>
    </figure>
  );
}
