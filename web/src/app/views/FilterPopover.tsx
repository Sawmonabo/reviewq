import type { ReactNode } from "react";
import { Icon } from "../../components/Icon";
import { Popover } from "../../components/Popover";

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
  return (
    <Popover
      label={popupLabel}
      className={`filter-popup ${popupClass}`}
      trigger={{
        className: "filter-control",
        disabled,
        "aria-label": triggerLabel,
        children: (
          <>
            {icon}
            <span className="filter-label">{label}</span>
            <span className="filter-value" title={value}>
              {value}
            </span>
            <Icon name="chevron" size={12} />
          </>
        ),
      }}
    >
      {children}
    </Popover>
  );
}
