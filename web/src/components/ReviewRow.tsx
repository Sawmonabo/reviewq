import type { ReactNode } from "react";
import type { HostProviderDto } from "../api/generated/contracts";
import type { MergeRequestRow, UnmatchedTicket } from "../api/generated/contracts/queue";
import { formatActivityTime, reviewReasonLabel } from "../lib/presentation";
import { Icon } from "./Icon";

const nativeActivityForReason: Readonly<
  Partial<Record<string, NonNullable<MergeRequestRow["focusedActivity"]>>>
> = {
  agent_input: "blocked",
  agent_failed: "failed",
  lead_working: "working",
  session_working: "working",
  working: "working",
};

export function providerLabel(provider: HostProviderDto): string {
  return provider === "claude" ? "Claude Code" : "Codex";
}

export function ReviewRow({
  row,
  selected,
  onOpen,
  selection,
  detail,
  disabled = false,
}: {
  readonly row: MergeRequestRow;
  readonly selected: boolean;
  readonly onOpen: () => void;
  readonly selection?: { readonly checked: boolean; readonly onToggle: () => void };
  readonly detail?: ReactNode;
  readonly disabled?: boolean;
}) {
  const status = row.reasonKind ?? row.runStage ?? row.queueState;
  const tracked = row.registeredLeadCount > 0;
  const currentReport = row.fullMrReviewedSha === row.sourceSha;
  const changed = row.mrState === "open" && row.fullMrReviewedSha && !currentReport;
  const nativeStatusShown =
    row.mrState === "open" && row.focusedActivity === nativeActivityForReason[status];
  const showReason =
    (tracked || (status !== "needs_review" && status !== "all_open")) &&
    !nativeStatusShown &&
    row.currentDisposition !== status &&
    !(currentReport && status === "report_ready") &&
    !(changed && status === "followup");
  return (
    <button
      type="button"
      className="review-row"
      aria-current={(!selection && selected) || undefined}
      role={selection ? "checkbox" : undefined}
      aria-checked={selection?.checked}
      aria-label={
        selection
          ? `Select ${row.tickets.map((ticket) => ticket.key).join(" · ") || row.projectPath} · !${row.iid}`
          : undefined
      }
      disabled={disabled}
      onClick={selection?.onToggle ?? onOpen}
    >
      {selection && (
        <span className="row-checkbox" aria-hidden="true">
          {selection.checked && <Icon name="check" size={13} />}
        </span>
      )}
      <div className="review-main">
        <div className="row-top">
          {row.tickets.length > 0 && (
            <span className="ticket-key">
              {row.tickets.map((ticket) => ticket.key).join(" · ")}
            </span>
          )}
          <span className="mr-id" title={row.projectPath}>
            {row.projectPath.split("/").at(-1)} !{row.iid}
          </span>
          {row.own && <span className="row-tag">Yours</span>}
          {row.draft && <span className="row-tag">Draft</span>}
          <ReviewBadges review={row} />
        </div>
        <strong className="row-title">{row.tickets[0]?.title || row.title}</strong>
        <div className="row-foot">
          {row.focusedProvider && (
            <span className="provider-pill">{providerLabel(row.focusedProvider)}</span>
          )}
          {!tracked && <span>No review run started</span>}
          <ReviewEvidenceBadges review={row} />
          {showReason && (
            <span className="row-tag" data-review-kind={status}>
              {reviewReasonLabel(status)}
            </span>
          )}
          {currentReport && (
            <span className="row-tag" data-review-kind="report_ready">
              Agent report ready
            </span>
          )}
          {changed && (
            <span className="row-tag" data-review-kind="code_changed">
              New commits need review
            </span>
          )}
          {detail}
        </div>
      </div>
      <ReviewPerson
        name={row.author?.label}
        role="Author"
        updatedAt={row.updatedAt}
        arrow={!selection}
      />
    </button>
  );
}

export function UnmatchedReviewRow({
  row,
  selected = false,
  disabled = false,
  onOpen,
}: {
  readonly row: UnmatchedTicket;
  readonly selected?: boolean;
  readonly disabled?: boolean;
  readonly onOpen: () => void;
}) {
  return (
    <button
      type="button"
      className="review-row"
      aria-current={selected || undefined}
      disabled={disabled}
      onClick={onOpen}
    >
      <div className="review-main">
        <div className="row-top">
          <span className="ticket-key">{row.key}</span>
          <span>Jira ticket</span>
          <span className="row-tag">{row.status}</span>
        </div>
        <strong className="row-title">{row.title}</strong>
        <div className="row-foot">
          <span className="row-tag" data-review-kind="thread_reply">
            <Icon name="warning" size={12} /> No matching MR
          </span>
        </div>
      </div>
      <ReviewPerson name={row.assignee ?? undefined} role="Assignee" arrow />
    </button>
  );
}

export function ReviewBadges({
  review,
}: {
  readonly review: Pick<MergeRequestRow, "personalApproval" | "ourComments" | "unreadReplies">;
}) {
  return (
    <>
      {review.personalApproval && (
        <span className="row-tag" data-review-kind="reviewed">
          You approved
        </span>
      )}
      {review.ourComments > 0 && (
        <span className="row-tag" data-review-kind="commented">
          Commented · {review.ourComments}
        </span>
      )}
      {review.unreadReplies > 0 && (
        <span className="row-tag" data-review-kind="thread_reply">
          New {review.unreadReplies === 1 ? "reply" : "replies"} · {review.unreadReplies}
        </span>
      )}
    </>
  );
}

function ReviewEvidenceBadges({ review }: { readonly review: MergeRequestRow }) {
  const phase = review.focusedActivity;
  const disposition = review.currentDisposition;
  const kind = phase === "blocked" ? "agent_input" : phase === "failed" ? "agent_failed" : phase;
  return (
    <>
      {review.mrState === "open" && phase && (
        <span className="native-status" data-review-kind={kind}>
          {
            {
              idle: "Idle",
              working: "Working",
              blocked: review.reasonKind === "agent_input" ? "Awaiting input" : "Blocked",
              failed: "Agent failed",
            }[phase]
          }
        </span>
      )}
      {disposition && (
        <span className="row-tag" data-review-kind={disposition}>
          {
            {
              reviewed: "✓ Reviewed",
              waiting_author: "Waiting on author",
              needs_review: "Needs review",
            }[disposition]
          }
        </span>
      )}
      {review.unreadNonReplyCount > 0 && (
        <span className="row-tag" data-review-kind="discussion_update">
          {review.unreadNonReplyCount} {review.unreadReplies > 0 ? "other updates" : "unread"}
        </span>
      )}
    </>
  );
}

function ReviewPerson({
  name,
  role,
  updatedAt,
  arrow,
}: {
  readonly name?: string | undefined;
  readonly role: "Author" | "Assignee";
  readonly updatedAt?: string;
  readonly arrow: boolean;
}) {
  const words = name?.trim().split(/\s+/).filter(Boolean) ?? [];
  const initials =
    words.length > 1
      ? `${words[0]?.charAt(0) ?? ""}${words.at(-1)?.charAt(0) ?? ""}`
      : words[0]?.slice(0, 2);
  const label = name?.trim() || (role === "Author" ? "Not available" : "Unassigned");
  return (
    <div className="row-person" aria-label={`${role}: ${label}`}>
      <span className="avatar" aria-hidden="true">
        {initials?.toUpperCase() || "—"}
      </span>
      <div className="row-person-name">
        <strong title={label}>{label}</strong>
        <small>
          {role}
          {updatedAt && ` · ${formatActivityTime(updatedAt)}`}
        </small>
      </div>
      {arrow && (
        <span className="row-arrow" aria-hidden="true">
          ›
        </span>
      )}
    </div>
  );
}
