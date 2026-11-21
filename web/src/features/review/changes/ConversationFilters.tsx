import { useId } from "react";
import type { RefObject } from "react";
import { Icon } from "../../../components/Icon";
import "./conversation-filters.css";

export type ConversationFilterValues = {
  readonly involving: boolean;
  readonly status: "any" | "open" | "resolved";
};

export const DEFAULT_FILTERS: ConversationFilterValues = { involving: false, status: "any" };

export function ConversationFilters({
  value,
  onChange,
  trigger,
}: {
  readonly value: ConversationFilterValues;
  readonly onChange: (value: ConversationFilterValues) => void;
  readonly trigger: RefObject<HTMLButtonElement | null>;
}) {
  const id = useId();
  const active = Number(value.involving) + Number(value.status !== "any");
  const description = [
    value.involving && "Involving you",
    value.status === "open" && "Open",
    value.status === "resolved" && "Resolved",
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <div className="conversation-filters">
      <button
        type="button"
        ref={trigger}
        className="conversation-filter-trigger"
        popoverTarget={id}
        aria-haspopup="dialog"
        aria-label={active ? `Filters, ${description}` : "Filters"}
        title={active ? description : "Filter conversations"}
        data-active={active || undefined}
      >
        <Icon name="filters" size={15} />
        <span>Filters</span>
        {active > 0 && <small>{active}</small>}
      </button>
      <section
        id={id}
        role="dialog"
        aria-labelledby={`${id}-title`}
        popover="auto"
        className="changes-popover conversation-filter-panel"
        onToggle={(event) => {
          if (event.newState === "open")
            event.currentTarget
              .querySelector<HTMLInputElement>("input:checked")
              ?.focus({ preventScroll: true });
        }}
      >
        <header>
          <h3 id={`${id}-title`}>Conversation filters</h3>
          <button
            type="button"
            disabled={!active}
            onClick={(event) => {
              event.currentTarget.closest<HTMLElement>("[popover]")?.hidePopover();
              onChange(DEFAULT_FILTERS);
              trigger.current?.focus({ preventScroll: true });
            }}
          >
            Reset
          </button>
        </header>
        <fieldset>
          <legend>Participants</legend>
          <div>
            {([false, true] as const).map((involving) => (
              <label key={String(involving)}>
                <input
                  type="radio"
                  name={`${id}-participants`}
                  checked={value.involving === involving}
                  onChange={() => onChange({ ...value, involving })}
                />
                <span>{involving ? "Involving you" : "Everyone"}</span>
              </label>
            ))}
          </div>
        </fieldset>
        <fieldset>
          <legend>Status</legend>
          <div>
            {(["any", "open", "resolved"] as const).map((status) => (
              <label key={status}>
                <input
                  type="radio"
                  name={`${id}-status`}
                  checked={value.status === status}
                  onChange={() => onChange({ ...value, status })}
                />
                <span>{status === "any" ? "Any" : status === "open" ? "Open" : "Resolved"}</span>
              </label>
            ))}
          </div>
        </fieldset>
      </section>
    </div>
  );
}
