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

const clockTimeFormat = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });

export function formatClockTime(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? "Unknown time" : clockTimeFormat.format(date);
}

export function readable(value: string): string {
  const words = value.replaceAll("_", " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

const reviewReasons: Readonly<Record<string, string>> = {
  needs_input: "Awaiting input",
  waiting_author: "Waiting on author",
  reviewed: "✓ Reviewed",
  followup: "Needs re-review",
  archive: "Archived",
  closed_mr: "MR closed",
  agent_input: "Agent needs input",
  permission_required: "Permission required",
  lead_working: "Lead working",
  agent_failed: "Agent error",
  session_working: "Session working",
  action_verifying: "Verifying review action",
  report_failed: "Report update failed",
  followup_ready: "Follow-up ready",
  recovery_pending: "Recovery pending",
  recovery_required: "Recovery required",
  recovery_uncertain: "Recovery uncertain",
  delivery_uncertain: "Delivery uncertain",
  action_retry: "Waiting to retry action",
  action_needs_input: "Review action needs input",
  approval_withdrawn: "Approval withdrawn",
  followup_pending: "Follow-up pending",
  followup_working: "Follow-up in progress",
  discussion_update: "Discussion updated",
  awaiting_merge: "Reviewed · awaiting merge",
  report_ready: "Agent report ready",
  decision_required: "Review decision needed",
};

export function reviewReasonLabel(reason: string): string {
  return reviewReasons[reason] ?? readable(reason);
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
