export type MatchRange = readonly [start: number, end: number];

export function pathMatch(path: string, query: string): readonly MatchRange[] | undefined {
  if (!query) return [];
  const terms = query.toLowerCase().split(/\s+/);
  let folded = "";
  const offsets: number[] = [];
  let offset = 0;
  for (const scalar of path) {
    const part = scalar.toLowerCase();
    folded += part;
    for (let index = 0; index < part.length; index++) offsets.push(offset);
    offset += scalar.length;
  }
  offsets.push(offset);
  const matches: MatchRange[] = [];
  if (terms.every((term) => folded.includes(term))) {
    for (const term of terms) {
      for (
        let start = folded.indexOf(term);
        start >= 0;
        start = folded.indexOf(term, start + term.length)
      ) {
        matches.push([start, start + term.length]);
      }
    }
  } else {
    const term = terms[0];
    if (terms.length !== 1 || !term || term.length < 2) return;
    let next = folded.lastIndexOf("/") + 1;
    for (const scalar of term) {
      const start = folded.indexOf(scalar, next);
      if (start < 0) return;
      next = start + scalar.length;
      matches.push([start, next]);
    }
  }
  return matches
    .map(([start, end]): MatchRange => {
      const from = offsets[start]!;
      const last = offsets[end - 1]!;
      return [from, last + (path.codePointAt(last)! > 0xffff ? 2 : 1)];
    })
    .sort((a, b) => a[0] - b[0]);
}

export function splitPath(path: string) {
  const cut = path.lastIndexOf("/") + 1;
  return { folder: path.slice(0, Math.max(0, cut - 1)), name: path.slice(cut), cut };
}
