// Adapted from Agent Overflow's standalone Markdown splitter. See NOTICE.
import { BoundaryDetector } from "./BoundaryDetector";
import { createInitialContext } from "./detector";

/** Suggest the source offset after the last potentially stable line. */
export function stableBoundary(source: string): number {
  const lines = source.split("\n");
  const detector = new BoundaryDetector();
  const boundary = detector.findStableBoundary(lines, createInitialContext());
  let offset = 0;
  for (let index = 0; index <= boundary; index++) {
    offset += (lines[index]?.length ?? 0) + 1;
  }
  return Math.min(offset, source.length);
}
