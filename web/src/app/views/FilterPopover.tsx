import { useId, useState } from "react";
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
  const triggerId = `${id}-trigger`;
  const [open, setOpen] = useState(false);
  function close() {
    document.getElementById(id)?.hidePopover();
    document.getElementById(triggerId)?.focus({ preventScroll: true });
  }
  return (
    <>
      <button
        id={triggerId}
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
