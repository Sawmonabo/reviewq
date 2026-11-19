const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export type PatchDocument = {
  readonly host: HTMLDivElement;
  readonly line: (key: string) => HTMLElement | undefined;
  readonly anchor: (key: string) => HTMLElement | undefined;
};

export function mountPatchDocument(host: HTMLDivElement, text: string): PatchDocument {
  const root = document.createDocumentFragment();
  const lines = new Map<string, HTMLElement>();
  const anchors = new Map<HTMLElement, HTMLElement>();
  let oldLine = 0;
  let newLine = 0;
  let inHunk = false;

  // The original line-ending bytes stay in the selectable text nodes.
  const physicalRows = text.matchAll(/[^\n]*\n|[^\n]+$/g);
  for (const [physical] of physicalRows) {
    const prefix = physical[0];
    const hunk = HUNK.exec(physical);
    const row = document.createElement("span");
    row.className = "patch-row";
    let oldKey = "";
    let newKey = "";

    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      inHunk = true;
      row.classList.add("patch-hunk");
    } else if (inHunk) {
      if (prefix === "+" || prefix === " ") {
        newKey = String(newLine++);
        lines.set(`new:${newKey}`, row);
      }
      if (prefix === "-" || prefix === " ") {
        oldKey = String(oldLine++);
        lines.set(`old:${oldKey}`, row);
      }
      if (prefix === "+") row.classList.add("patch-add");
      if (prefix === "-") row.classList.add("patch-delete");
    }

    if (oldKey || newKey) row.dataset.line = newKey ? `new:${newKey}` : `old:${oldKey}`;
    row.dataset.old = oldKey;
    row.dataset.new = newKey;
    row.append(document.createTextNode(physical));
    root.append(row);
  }
  host.replaceChildren(root);
  return {
    host,
    line: (key) => lines.get(key),
    anchor: (key) => {
      const row = lines.get(key);
      if (!row) return undefined;
      let target = anchors.get(row);
      if (!target) {
        target = document.createElement("span");
        target.className = "patch-thread-slot";
        row.after(target);
        anchors.set(row, target);
      }
      return target;
    },
  };
}
