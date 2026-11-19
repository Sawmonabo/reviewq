import { useId, useRef, useState } from "react";
import type { ReactNode } from "react";
import { Icon } from "../../components/Icon";

export function FilterPopover({
  label,
  value,
  triggerLabel,
  popupLabel,
  popupClass,
  disabled,
  icon,
  children,
}: {
  readonly label: string;
  readonly value: string;
  readonly triggerLabel: string;
  readonly popupLabel: string;
  readonly popupClass: string;
  readonly disabled?: boolean;
  readonly icon?: ReactNode;
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
        className="filter-control"
        disabled={disabled}
        aria-label={triggerLabel}
        aria-haspopup="dialog"
        aria-expanded={open}
        popoverTarget={id}
      >
        {icon}
        <span className="filter-label">{label}</span>
        <span className="filter-value" title={value}>
          {value}
        </span>
        <Icon name="chevron" size={12} />
      </button>
      <section
        id={id}
        ref={popup}
        popover="auto"
        role="dialog"
        aria-label={popupLabel}
        className={`filter-popup ${popupClass}`}
        onToggle={(event) => setOpen(event.newState === "open")}
      >
        {open && children(close)}
      </section>
    </>
  );
}
