import { lazy, Suspense, useRef, useState } from "react";
import {
  localDateRange,
  localDay,
  parseLocal,
  presets,
} from "../../../features/queue/archive-filters";
import type { DatePreset } from "../../../features/queue/archive-filters";
import { Icon } from "../../../components/Icon";
import { FilterPopover } from "../FilterPopover";
import "./archive.css";

const ArchiveCalendar = lazy(() => import("./ArchiveCalendar"));
const displayDate = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });

type Value = {
  readonly preset: DatePreset;
  readonly start: string;
  readonly end: string;
};

export function ArchiveDateFilter({
  value,
  onChange,
}: {
  readonly value: Value;
  readonly onChange: (value: Value) => void;
}) {
  const start = parseLocal(value.start);
  const end = parseLocal(value.end);
  const label =
    value.preset === "custom" && start && end
      ? `${displayDate.format(start)} – ${displayDate.format(end)}`
      : presets[value.preset];
  return (
    <FilterPopover
      label="Review date"
      value={label}
      triggerLabel="Filter by review date"
      popupLabel="Review date range"
      popupClass="archive-date-popover"
      icon={<Icon name="calendar" size={15} />}
    >
      {(close) => <DateRange value={value} onChange={onChange} close={close} />}
    </FilterPopover>
  );
}

function DateRange({
  value,
  onChange,
  close,
}: {
  readonly value: Value;
  readonly onChange: (value: Value) => void;
  readonly close: () => void;
}) {
  const [draft, setDraft] = useState(() => fieldsFor(value));
  const [error, setError] = useState(false);
  const fromInput = useRef<HTMLInputElement>(null);
  const toInput = useRef<HTMLInputElement>(null);

  function choose(preset: DatePreset) {
    if (preset === "custom") {
      setDraft((current) => ({ ...current, preset }));
      fromInput.current?.focus();
    } else {
      onChange({ preset, start: "", end: "" });
      close();
    }
  }

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        const validStart = parseLocal(draft.start);
        const validEnd = parseLocal(draft.end);
        if (!validStart || !validEnd || validStart > validEnd) {
          setError(true);
          (!validStart ? fromInput : toInput).current?.focus();
          return;
        }
        onChange({ ...draft, preset: "custom" });
        close();
      }}
    >
      <div className="archive-date-layout">
        <div className="archive-date-presets" aria-label="Quick date ranges">
          <span>Review date</span>
          {(Object.keys(presets) as DatePreset[]).map((preset) => (
            <button
              key={preset}
              type="button"
              aria-pressed={preset === draft.preset}
              autoFocus={preset === value.preset}
              onClick={() => choose(preset)}
            >
              {presets[preset]}
            </button>
          ))}
        </div>
        <div className="archive-custom-dates">
          <div className="archive-range-fields">
            <label>
              From
              <input
                type="date"
                aria-label="Reviewed from"
                ref={fromInput}
                value={draft.start}
                onChange={(event) => {
                  setDraft({ ...draft, preset: "custom", start: event.currentTarget.value });
                  setError(false);
                }}
              />
            </label>
            <span aria-hidden="true">→</span>
            <label>
              To
              <input
                type="date"
                aria-label="Reviewed through"
                ref={toInput}
                value={draft.end}
                onChange={(event) => {
                  setDraft({ ...draft, preset: "custom", end: event.currentTarget.value });
                  setError(false);
                }}
              />
            </label>
          </div>
          <Suspense fallback={<p className="meta archive-calendar-loading">Loading calendar…</p>}>
            <ArchiveCalendar
              start={draft.start}
              end={draft.end}
              onChange={(start, end) => {
                setDraft({ preset: "custom", start, end });
                setError(false);
              }}
            />
          </Suspense>
          {error && (
            <p className="archive-date-error" role="alert">
              Choose an end date on or after the start date.
            </p>
          )}
        </div>
      </div>
      <div className="archive-popup-footer">
        <span>Based on review date</span>
        <button type="button" className="quiet-button" onClick={close}>
          Cancel
        </button>
        <button type="submit" className="primary">
          Apply range
        </button>
      </div>
    </form>
  );
}

function fieldsFor(value: Value): Value {
  if (value.preset === "custom") return value;
  const dates = localDateRange(value.preset, "", "");
  const through = dates.untilExclusive ? new Date(dates.untilExclusive) : undefined;
  if (through) through.setDate(through.getDate() - 1);
  return {
    preset: value.preset,
    start: dates.fromInclusive ? localDay(new Date(dates.fromInclusive)) : "",
    end: through ? localDay(through) : "",
  };
}
