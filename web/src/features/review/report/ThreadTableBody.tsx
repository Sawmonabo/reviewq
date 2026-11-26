import { createContext, useContext, useLayoutEffect, useMemo, useRef } from "react";
import type { ComponentProps, ReactNode } from "react";
import { flushSync } from "react-dom";
import type { Element } from "hast";
import type { ExtraProps } from "hast-util-to-jsx-runtime";
import { Icon } from "../../../components/Icon";

type ThreadFolds = {
  // The report offset where this group's source text starts.
  readonly start: number;
  // The report offsets of the thread tables the reader opened.
  readonly open: ReadonlySet<number>;
  readonly toggle: (table: number) => void;
};
const ThreadFoldContext = createContext<ThreadFolds | undefined>(undefined);

// Gives one group's table bodies the fold state. The value changes only with a fold or the
// group, so the bodies draw again only then.
export function ThreadFoldScope({
  start,
  open,
  toggle,
  children,
}: ThreadFolds & { readonly children: ReactNode }) {
  const value = useMemo(() => ({ start, open, toggle }), [start, open, toggle]);
  return <ThreadFoldContext value={value}>{children}</ThreadFoldContext>;
}

// Draws a report table body. The previous-threads table folds its settled rows into a body
// that opens from the control body before it.
export function ThreadTableBody({
  node,
  children,
  ...props
}: ComponentProps<"tbody"> & ExtraProps) {
  const folds = useContext(ThreadFoldContext);
  const name = node?.properties.className;
  const part = Array.isArray(name) ? name[0] : undefined;
  const offset = node?.position?.start.offset;
  if (
    !folds ||
    !node ||
    offset === undefined ||
    (part !== "report-thread-control" && part !== "report-thread-settled")
  )
    return <tbody {...props}>{children}</tbody>;
  const table = folds.start + offset;
  const open = folds.open.has(table);
  const id = `report-settled-${table}`;
  const toggle = () => folds.toggle(table);
  return part === "report-thread-settled" ? (
    <SettledRows id={id} open={open} onOpen={toggle}>
      {children}
    </SettledRows>
  ) : (
    <FoldControl node={node} id={id} open={open} onToggle={toggle} />
  );
}

// Each settled cell's wrapper is hidden until browser find reaches it. React writes `hidden`
// as a boolean, so the attribute is set here after every render of the body, which also covers
// wrappers drawn again. The browser reveals only the wrapper it matched; its `beforematch` event
// opens the fold in the same commit that hides the wrappers, and the attribute then follows the
// fold state, which hides a revealed wrapper again when the reader closes the fold. While closed,
// the empty row shells are hidden from assistive technology.
function SettledRows({
  id,
  open,
  onOpen,
  children,
}: {
  readonly id: string;
  readonly open: boolean;
  readonly onOpen: () => void;
  readonly children?: ReactNode;
}) {
  const body = useRef<HTMLTableSectionElement>(null);
  const wasOpen = useRef(open);
  useLayoutEffect(() => {
    for (const cell of body.current?.querySelectorAll(".report-thread-cell") ?? [])
      if (open) cell.removeAttribute("hidden");
      else cell.setAttribute("hidden", "until-found");
  });
  useLayoutEffect(() => {
    const element = body.current;
    if (open || !element) return;
    // Open before the browser scrolls to the match, so the rows above it have their height.
    const reveal = () => flushSync(onOpen);
    element.addEventListener("beforematch", reveal);
    return () => element.removeEventListener("beforematch", reveal);
  }, [open, onOpen]);
  // The rows fade in when the reader opens them, not when opened rows draw again. The marker
  // stays only while the stylesheet's fade runs; it runs none with reduced motion, and a fade
  // cancelled by a change of that setting settles too.
  useLayoutEffect(() => {
    const element = body.current;
    const opening = open && !wasOpen.current;
    wasOpen.current = open;
    if (!element || !opening) return;
    let current = true;
    const done = () => {
      if (current) delete element.dataset.opening;
    };
    element.dataset.opening = "";
    const fades = element.getAnimations({ subtree: true }).map((fade) => fade.finished);
    void Promise.allSettled(fades).then(done);
    return () => {
      done();
      current = false;
    };
  }, [open]);
  return (
    <tbody
      ref={body}
      className="report-thread-settled"
      id={id}
      data-open={open || undefined}
      aria-hidden={!open || undefined}
    >
      {children}
    </tbody>
  );
}

function FoldControl({
  node,
  id,
  open,
  onToggle,
}: {
  readonly node: Element;
  readonly id: string;
  readonly open: boolean;
  readonly onToggle: () => void;
}) {
  return (
    <tbody className="report-thread-control">
      <tr>
        <td colSpan={Number(node.properties.dataReportColumns)}>
          <div className="report-thread-line">
            <button
              type="button"
              className="report-thread-pill"
              aria-expanded={open}
              aria-controls={id}
              onClick={onToggle}
            >
              <Icon name="check" size={12} />
              <b>{Number(node.properties.dataReportSettled)} settled</b>{" "}
              <span className="report-thread-action">
                <span>{open ? "Hide" : "Show"}</span>
              </span>
              <Icon name="chevron" size={12} />
            </button>
          </div>
        </td>
      </tr>
    </tbody>
  );
}
