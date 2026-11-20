const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;
const FILE_LABELS = /^--- [^\n]*\n\+\+\+ [^\n]*\n(?=@@ )/;
const ROWS_PER_BLOCK = 96;
const ROW_HEIGHT = 21;

type Row = {
  readonly text: string;
  readonly kind: string;
  readonly oldLine?: number;
  readonly newLine?: number;
};
type Block = {
  readonly element: HTMLElement;
  rows: Row[] | undefined;
};

export type PatchDocument = {
  readonly line: (side: "old" | "new", number: number) => HTMLElement | undefined;
  readonly anchor: (key: string) => HTMLElement | undefined;
  readonly release: () => void;
};

export function mountPatchDocument(host: HTMLDivElement, text: string): PatchDocument {
  const rows = readRows(text);
  const blocks: Block[] = [];
  const anchors = new Map<HTMLElement, HTMLElement>();
  const oldLines = new Uint32Array(rows.length);
  const newLines = new Uint32Array(rows.length);
  const root = document.createDocumentFragment();
  const grouped = rows.length > ROWS_PER_BLOCK * 2;
  const scroller = grouped ? host.closest<HTMLElement>(".drawer-body") : null;
  const initialBlocks: HTMLElement[] = [];
  let frame = 0;
  let visibilityFrame = 0;
  let next = 0;
  const observer = grouped
    ? new IntersectionObserver(prepareVisible, {
        root: scroller,
        rootMargin: "300px 0px",
      })
    : undefined;

  for (let start = 0; start < rows.length; start += ROWS_PER_BLOCK) {
    const element = document.createElement("div");
    element.className = grouped ? "patch-block patch-block-deferred" : "patch-block";
    element.dataset.patchBlock = String(blocks.length);
    const part = rows.slice(start, start + ROWS_PER_BLOCK);
    if (grouped) element.style.setProperty("--patch-block-height", `${part.length * ROW_HEIGHT}px`);
    blocks.push({ element, rows: part });
    root.append(element);
  }
  rows.forEach((row, ordinal) => {
    if (row.oldLine !== undefined) oldLines[ordinal] = row.oldLine + 1;
    if (row.newLine !== undefined) newLines[ordinal] = row.newLine + 1;
  });
  if (observer) {
    fillVisibleBlocks();
    for (const block of blocks) if (block.rows) observer.observe(block.element);
    frame = requestAnimationFrame(prepareNext);
  } else {
    blocks.forEach(build);
    host.dataset.patchPrepared = "";
  }
  host.replaceChildren(root);

  function fillVisibleBlocks() {
    const scrollTop = scroller ? scroller.getBoundingClientRect().top + scroller.clientTop : 0;
    const viewportBottom =
      scrollTop + (scroller?.clientHeight ?? document.documentElement.clientHeight);
    const controls = host.closest(".file-stream")?.querySelector(".file-stream-controls");
    const viewportTop = Math.max(scrollTop, controls?.getBoundingClientRect().bottom ?? scrollTop);
    if (viewportTop >= viewportBottom) return;
    const contentTop = host.getBoundingClientRect().top + host.clientTop;
    const blockHeight = ROWS_PER_BLOCK * ROW_HEIGHT;
    const first = Math.max(0, Math.floor((viewportTop - contentTop) / blockHeight));
    // Deferred blocks still have their estimated height. Fill the part of this
    // estimate in view without waiting for the observer or the next frame.
    for (let index = first; index < first + 2; index++) {
      const block = blocks[index];
      const top = contentTop + index * blockHeight;
      if (!block?.rows || top >= viewportBottom) break;
      if (top + block.rows.length * ROW_HEIGHT > viewportTop) {
        build(block);
        block.element.style.contentVisibility = "visible";
        initialBlocks.push(block.element);
      }
    }
  }

  function build(block: Block) {
    if (!block.rows) return;
    const fragment = document.createDocumentFragment();
    for (const row of block.rows) {
      const element = document.createElement("span");
      element.className = row.kind ? `patch-row ${row.kind}` : "patch-row";
      if (row.newLine !== undefined) element.dataset.new = String(row.newLine);
      if (row.oldLine !== undefined) element.dataset.old = String(row.oldLine);
      element.append(document.createTextNode(row.text));
      fragment.append(element);
    }
    block.element.append(fragment);
    block.rows = undefined;
    observer?.unobserve(block.element);
    block.element.classList.remove("patch-block-deferred");
  }

  function prepareVisible(entries: IntersectionObserverEntry[]) {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      const block = blocks[Number((entry.target as HTMLElement).dataset.patchBlock)];
      if (block) build(block);
    }
  }

  function prepareNext() {
    if (initialBlocks.length && !visibilityFrame) {
      // Give the seeded rows their first frame before the browser can skip their layout.
      visibilityFrame = requestAnimationFrame(() => {
        for (const block of initialBlocks) block.style.removeProperty("content-visibility");
        initialBlocks.length = 0;
      });
    }
    // Bounded DOM work per frame. Once prepared, native Find can search every
    // block, including those Chromium has skipped for layout and paint.
    for (let prepared = 0; next < blocks.length && prepared < 4; next++) {
      const block = blocks[next];
      if (!block?.rows) continue;
      build(block);
      prepared++;
    }
    if (next < blocks.length) frame = requestAnimationFrame(prepareNext);
    else host.dataset.patchPrepared = "";
  }

  function rowAt(side: "old" | "new", line: number) {
    const ordinal = (side === "old" ? oldLines : newLines).indexOf(line + 1);
    if (ordinal < 0) return;
    const block = blocks[Math.floor(ordinal / ROWS_PER_BLOCK)];
    if (!block) return;
    build(block);
    return block.element.querySelector<HTMLElement>(`[data-${side}="${line}"]`) ?? undefined;
  }

  return {
    line: rowAt,
    anchor(key) {
      const [side, value] = key.split(":");
      if (side !== "old" && side !== "new") return;
      const row = rowAt(side, Number(value));
      if (!row) return;
      let target = anchors.get(row);
      if (!target) {
        target = document.createElement("span");
        target.className = "patch-thread-slot";
        row.after(target);
        anchors.set(row, target);
      }
      return target;
    },
    release() {
      cancelAnimationFrame(frame);
      cancelAnimationFrame(visibilityFrame);
      observer?.disconnect();
      host.replaceChildren();
      delete host.dataset.patchPrepared;
    },
  };
}

function readRows(text: string): Row[] {
  let oldLine = 0;
  let newLine = 0;
  let inHunk = false;
  const rows: Row[] = [];
  // Preserve the physical line endings inside the selectable text nodes.
  const physicalRows = text.replace(FILE_LABELS, "").matchAll(/[^\n]*\n|[^\n]+$/g);
  for (const [physical] of physicalRows) {
    const prefix = physical[0];
    const hunk = HUNK.exec(physical);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      inHunk = true;
      rows.push({ text: physical, kind: "patch-hunk" });
    } else {
      const old = inHunk && (prefix === "-" || prefix === " ") ? oldLine++ : undefined;
      const next = inHunk && (prefix === "+" || prefix === " ") ? newLine++ : undefined;
      rows.push({
        text: physical,
        kind:
          inHunk && prefix === "+" ? "patch-add" : inHunk && prefix === "-" ? "patch-delete" : "",
        ...(old === undefined ? {} : { oldLine: old }),
        ...(next === undefined ? {} : { newLine: next }),
      });
    }
  }
  return rows;
}
