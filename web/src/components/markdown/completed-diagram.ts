import { decodeUtf8, readBoundedBytes } from "../../api/bounded-bytes";
import { request } from "../../api/client";
import type { DrawingHint } from "../../api/generated/contracts/common";
import type { DiagramCache } from "./diagram-cache";
import { parseDiagram, renderDiagram } from "./mermaid";
import {
  diagramFontIdentity,
  digestDrawing,
  DRAWING_RECIPE,
  MAX_SAVED_SVG_BYTES,
  normalizeDrawing,
  SAVED_DIAGRAM_ID,
  savedDiagram,
} from "./saved-diagram";

async function retrieve(
  source: string,
  hints: readonly DrawingHint[] | undefined,
  font: string,
  signal: AbortSignal,
) {
  if (!hints?.length) return;
  const hash = await digestDrawing(source);
  const candidates = hints.filter(
    (drawing) => drawing.sourceSha256 === hash && drawing.recipe === DRAWING_RECIPE,
  );
  if (!candidates.length) return;
  const hint = candidates.find((drawing) => drawing.font === font);
  if (!hint) return;
  const deadline = AbortSignal.any([signal, AbortSignal.timeout(250)]);
  const response = await request(`/drawings/${encodeURIComponent(hint.reference)}`, {
    signal: deadline,
  });
  if (!response.headers.get("content-type")?.startsWith("image/svg+xml")) return;
  const svg = decodeUtf8(await readBoundedBytes(response, MAX_SAVED_SVG_BYTES));
  savedDiagram(svg, SAVED_DIAGRAM_ID);
  return svg;
}

export async function completedDiagram(
  id: string,
  source: string,
  hints: readonly DrawingHint[] | undefined,
  cache: DiagramCache | null,
) {
  if (!cache) return parseDiagram(await renderDiagram(id, source));
  const theme = document.documentElement.dataset.theme ?? "";
  const font = await diagramFontIdentity().catch(() => undefined);
  if (!font) return parseDiagram(await renderDiagram(id, source));
  const cached = await cache.acquire(
    JSON.stringify([DRAWING_RECIPE, theme, font, source]),
    async (signal) => {
      const ready = await retrieve(source, hints, font, signal).catch(() => undefined);
      signal.throwIfAborted();
      if (ready) return { svg: ready, id: SAVED_DIAGRAM_ID, reusable: ready };

      const svg = await renderDiagram(id, source);
      return { svg, id, reusable: normalizeDrawing(parseDiagram(svg).root) };
    },
  );
  if (typeof cached === "string") return savedDiagram(cached, id);
  if (cached.id === id) return parseDiagram(cached.svg);
  if (cached.reusable) return savedDiagram(cached.reusable, id);
  return parseDiagram(await renderDiagram(id, source));
}
