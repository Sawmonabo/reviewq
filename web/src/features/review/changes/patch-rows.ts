const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

type RowKind = "context" | "add" | "delete" | "hunk";
export type PatchRow = {
  readonly start: number;
  readonly end: number;
  readonly byte: number;
  readonly hunk: number | undefined;
  readonly kind: RowKind;
  readonly old: number | undefined;
  readonly new: number | undefined;
};

export function patchRows(text: string) {
  const result: PatchRow[] = [];
  let old = 0;
  let next = 0;
  let byte = 0;
  let hunk: number | undefined;
  for (const match of text.matchAll(/[^\n]*\n|[^\n]+$/g)) {
    const physical = match[0];
    const start = match.index;
    const header = HUNK.exec(physical);
    const marker = physical[0];
    let kind: RowKind = "context";
    let oldLine: number | undefined;
    let newLine: number | undefined;
    if (header) {
      old = Number(header[1]);
      next = Number(header[2]);
      hunk = byte;
      kind = "hunk";
    } else if (hunk !== undefined) {
      if (marker === "-" || marker === " ") oldLine = old++;
      if (marker === "+" || marker === " ") newLine = next++;
      if (marker === "-") kind = "delete";
      if (marker === "+") kind = "add";
    }
    result.push({
      start,
      end: start + physical.length,
      byte,
      hunk,
      kind,
      old: oldLine,
      new: newLine,
    });
    byte += utf8Length(physical);
  }
  return result;
}

export function utf8Length(text: string) {
  let bytes = 0;
  for (const scalar of text) bytes += scalarWidth(scalar);
  return bytes;
}

export function indexAtByte(text: string, target: number) {
  let bytes = 0;
  let offset = 0;
  for (const scalar of text) {
    if (bytes >= target) break;
    bytes += scalarWidth(scalar);
    offset += scalar.length;
  }
  return bytes === target ? offset : undefined;
}

function scalarWidth(scalar: string) {
  const point = scalar.codePointAt(0)!;
  return point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
}
