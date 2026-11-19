import { Icon } from "./Icon";

type Option = { readonly value: string; readonly label: string };

export function FilterSelect({
  label,
  accessibleLabel = label,
  value,
  options,
  onChange,
  disabled = false,
}: {
  readonly label: string;
  readonly accessibleLabel?: string;
  readonly value: string;
  readonly options: readonly Option[];
  readonly onChange: (value: string) => void;
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
        aria-label={accessibleLabel}
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.currentTarget.value)}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    </label>
  );
}
