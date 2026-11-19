export const presets = {
  all: "All time",
  week: "This week",
  seven: "Last 7 days",
  thirty: "Last 30 days",
  month: "This month",
  custom: "Custom range",
} as const;

export type DatePreset = keyof typeof presets;

export function localDateRange(preset: DatePreset, start: string, end: string) {
  if (preset === "all") return {};
  if (preset === "custom") {
    const from = parseLocal(start);
    const until = parseLocal(end);
    if (until) until.setDate(until.getDate() + 1);
    return {
      fromInclusive: from?.toISOString(),
      untilExclusive: until?.toISOString(),
      invalid: Boolean(from && until && from >= until),
    };
  }
  const now = new Date();
  const from = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const to = new Date(from);
  to.setDate(to.getDate() + 1);
  if (preset === "month") from.setDate(1);
  else if (preset === "week") from.setDate(from.getDate() - ((from.getDay() + 6) % 7));
  else from.setDate(from.getDate() - (preset === "seven" ? 6 : 29));
  return { fromInclusive: from.toISOString(), untilExclusive: to.toISOString() };
}

export function parseLocal(value: string) {
  if (!/^\d{4}-\d\d-\d\d$/.test(value)) return;
  const date = new Date(`${value}T00:00:00`);
  return localDay(date) === value ? date : undefined;
}

export function localDay(value: Date): string {
  const year = String(value.getFullYear()).padStart(4, "0");
  const month = String(value.getMonth() + 1).padStart(2, "0");
  return `${year}-${month}-${String(value.getDate()).padStart(2, "0")}`;
}
