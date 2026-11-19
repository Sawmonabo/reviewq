import { DayPicker } from "@daypicker/react";
import { Icon } from "../../../components/Icon";
import { localDay, parseLocal } from "../../../features/queue/archive-filters";
import "@daypicker/react/style.css";
import "./calendar.css";

export default function ArchiveCalendar({
  start,
  end,
  onChange,
}: {
  readonly start: string;
  readonly end: string;
  readonly onChange: (start: string, end: string) => void;
}) {
  const from = parseLocal(start);
  const to = parseLocal(end);
  return (
    <DayPicker
      aria-label="Review date calendar"
      mode="range"
      navLayout="around"
      weekStartsOn={1}
      defaultMonth={from ?? to ?? new Date()}
      selected={{ from, to }}
      resetOnSelect
      onSelect={(next) =>
        onChange(next?.from ? localDay(next.from) : "", next?.to ? localDay(next.to) : "")
      }
      components={{
        Chevron: ({ orientation }) => (
          <span className="archive-calendar-chevron" data-direction={orientation}>
            <Icon name="chevron" size={16} />
          </span>
        ),
      }}
    />
  );
}
