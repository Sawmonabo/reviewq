const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

export function formatTime(value: string | number): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "Unknown time" : dateFormat.format(date);
}

const activityTimeFormat = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

export function formatActivityTime(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "Unknown time" : activityTimeFormat.format(date);
}

export function readable(value: string): string {
  const words = value.replaceAll("_", " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export function safeUrl(value: string | null | undefined, base?: string): string | undefined {
  if (!value) return;
  try {
    const url = new URL(value, base);
    return ["https:", "http:", "mailto:"].includes(url.protocol) ? url.href : undefined;
  } catch {
    return undefined;
  }
}

export function describeError(error: unknown): string {
  return error instanceof Error ? error.message : "ReviewQ could not complete the request.";
}
