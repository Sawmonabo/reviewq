import { eligibleDiagram, parseDiagram, renderDiagram } from "./components/markdown/mermaid";
import {
  diagramFontIdentity,
  normalizeDrawing,
  SAVED_DIAGRAM_ID,
} from "./components/markdown/saved-diagram";

async function produce() {
  const capability = location.hash.slice(1);
  history.replaceState(null, "", location.pathname);
  if (!/^[A-Za-z0-9_-]{43}$/.test(capability)) return;
  const input = await fetch("/job/input", { headers: { Authorization: `Bearer ${capability}` } });
  if (!input.ok) return;
  const job: unknown = await input.json();
  if (
    typeof job !== "object" ||
    job === null ||
    !("source" in job) ||
    !("output" in job) ||
    typeof job.source !== "string" ||
    typeof job.output !== "string" ||
    !/^[A-Za-z0-9_-]{43}$/.test(job.output)
  )
    return;
  const output = await draw(job.source).catch(() => ({ kind: "declined" }));
  await fetch("/job/output", {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${job.output}` },
    body: JSON.stringify(output),
  });
}

async function draw(source: string) {
  if (eligibleDiagram(source)) {
    const font = await diagramFontIdentity();
    const official = await renderDiagram(SAVED_DIAGRAM_ID, source);
    const svg = normalizeDrawing(parseDiagram(official).root);
    if (svg) return { kind: "ready", svg, font };
  }
  return { kind: "declined" };
}

void produce().catch(() => {});
