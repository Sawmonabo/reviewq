import type { RootContent, Text } from "hast";

export type PositionDelta = { readonly line: number; readonly offset: number };

export const NEWLINE: Text = { type: "text", value: "\n" };

// The canonical parser measures positions in UTF-16 and starts every slice at line one.
export function shiftNodes(nodes: readonly RootContent[], delta: PositionDelta): number {
  let count = 0;
  for (const node of nodes) {
    count++;
    const { position } = node;
    if (position && (delta.offset || delta.line)) {
      const { start, end } = position;
      node.position = {
        start: {
          ...start,
          line: start.line + delta.line,
          offset: start.offset === undefined ? undefined : start.offset + delta.offset,
        },
        end: {
          ...end,
          line: end.line + delta.line,
          offset: end.offset === undefined ? undefined : end.offset + delta.offset,
        },
      };
    }
    if ("children" in node) count += shiftNodes(node.children, delta);
  }
  return count;
}
