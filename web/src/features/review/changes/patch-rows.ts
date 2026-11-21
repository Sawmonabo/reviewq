import type { PatchSourceRow } from "../../../api/generated/contracts";

const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

type RowKind = "context" | "add" | "delete" | "hunk";
export type PatchRow = {
  readonly start: number;
  readonly end: number;
  readonly byte: number;
  readonly byteEnd: number;
  readonly rowStart: number;
  readonly rowEnd: number;
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
    const byteEnd = byte + utf8Length(physical);
    result.push({
      start,
      end: start + physical.length,
      byte,
      byteEnd,
      rowStart: byte,
      rowEnd: byteEnd,
      hunk,
      kind,
      old: oldLine,
      new: newLine,
    });
    byte = byteEnd;
  }
  return result;
}

export function patchSourceRows(source: readonly PatchSourceRow[]): PatchRow[] {
  let start = 0;
  return source.map((row) => {
    const end = start + row.text.length;
    let kind: RowKind = "context";
    if (row.hunk === row.rowStart) kind = "hunk";
    else if (row.oldLine === null && row.newLine !== null) kind = "add";
    else if (row.newLine === null && row.oldLine !== null) kind = "delete";
    const result = {
      start,
      end,
      byte: row.sourceStart,
      byteEnd: row.sourceEnd,
      rowStart: row.rowStart,
      rowEnd: row.rowEnd,
      hunk: row.hunk ?? undefined,
      kind,
      old: row.oldLine ?? undefined,
      new: row.newLine ?? undefined,
    };
    start = end;
    return result;
  });
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
