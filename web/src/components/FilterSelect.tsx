import type { Ref } from "react";
import { Icon } from "./Icon";

type Option = { readonly value: string; readonly label: string; readonly disabled?: boolean };

export function FilterSelect({
  label,
  accessibleLabel = label,
  value,
  options,
  onChange,
  selectRef,
  disabled = false,
}: {
  readonly label: string;
  readonly accessibleLabel?: string;
  readonly value: string;
  readonly options: readonly Option[];
  readonly onChange: (value: string) => void;
  readonly selectRef?: Ref<HTMLSelectElement>;
  readonly disabled?: boolean;
}) {
  const shown = (options.find((option) => option.value === value) ?? options[0])?.label;
  return (
    <label className="filter-control select-filter">
      <span className="filter-label">{label}</span>
      <span className="filter-value" title={shown}>
        {shown}
      </span>
      <Icon name="chevron" size={12} />
      <select
        ref={selectRef}
        aria-label={accessibleLabel}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.currentTarget.value)}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value} disabled={option.disabled}>
            {option.label}
          </option>
        ))}
      </select>
    </label>
  );
}
