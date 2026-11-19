import { formatActivityTime, formatClockTime } from "../../../lib/presentation";

const exactTime = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  year: "numeric",
  hour: "numeric",
  minute: "2-digit",
  second: "2-digit",
  hour12: true,
  timeZoneName: "short",
});

export function HistoryTimestamp({
  timestamp,
  delivery = false,
  includeDate = false,
}: {
  readonly timestamp: string;
  readonly delivery?: boolean;
  readonly includeDate?: boolean;
}) {
  const date = new Date(timestamp);
  const detail = Number.isNaN(date.getTime()) ? "Unknown time" : exactTime.format(date);
  const label = delivery ? `Delivery recorded · ${detail}` : detail;

  return (
    <time dateTime={timestamp} title={`${label} · ${timestamp}`} aria-label={label}>
      {includeDate ? formatActivityTime(timestamp) : formatClockTime(timestamp)}
    </time>
  );
}
