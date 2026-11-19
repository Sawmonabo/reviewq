import { useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { Icon } from "../../../components/Icon";
import "./archive.css";

export function ArchivePopover({
  kind,
  label,
  value,
  children,
}: {
  readonly kind: "date" | "sprint";
  readonly label: string;
  readonly value: string;
  readonly children: (close: () => void) => ReactNode;
}) {
  const id = useId();
  const popup = useRef<HTMLElement>(null);
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  function close() {
    popup.current?.hidePopover();
    trigger.current?.focus({ preventScroll: true });
  }
  return (
    <>
      <button
        ref={trigger}
        type="button"
        className="filter-control archive-trigger"
        aria-label={kind === "date" ? "Filter by review date" : "Filter by sprint"}
        aria-haspopup="dialog"
        aria-expanded={open}
        popoverTarget={id}
      >
        <Icon name={kind === "date" ? "calendar" : "refresh"} size={15} />
        <span className="filter-label">{label}</span>
        <span className="filter-value">{value}</span>
        <Icon name="chevron" size={12} />
      </button>
      <section
        id={id}
        ref={popup}
        popover="auto"
        role="dialog"
        aria-label={kind === "date" ? "Review date range" : "Choose sprints"}
        className={`archive-popover archive-${kind}-popover`}
        onToggle={(event) => setOpen(event.newState === "open")}
      >
        {open && children(close)}
      </section>
    </>
  );
}
