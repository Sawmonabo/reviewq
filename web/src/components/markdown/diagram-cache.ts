import { createContext } from "react";

type Diagram = {
  readonly source: string;
  readonly theme: string;
  readonly id: string;
  readonly svg: string;
};

export const DiagramCacheContext = createContext<{
  readonly scope: string;
  diagram?: Diagram;
} | null>(null);

export function reusableDiagram(root: SVGSVGElement, id: string, sourceLength: number) {
  // Bound retained UTF-16 storage to 128 KiB, in addition to one eligible source.
  const limit = 64 * 1024;
  if (
    sourceLength > limit ||
    root.id !== id ||
    [...root.querySelectorAll("[id]")].some((element) => !element.id.startsWith(id))
  )
    return undefined;
  const svg = new XMLSerializer().serializeToString(root);
  return svg.length <= limit ? svg : undefined;
}
