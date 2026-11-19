import type { ReactNode } from "react";
import type { MergeRequestRow } from "../api/generated/contracts";
import { readable } from "../lib/presentation";
import { Icon } from "./Icon";

const reasons: Readonly<Record<string, readonly [string, string]>> = {
  needs_input: ["Awaiting input", "rose"],
  working: ["Working", "green"],
  waiting_author: ["Waiting on author", "amber"],
  needs_review: ["Needs review", "blue"],
  reviewed: ["Reviewed · awaiting merge", "green"],
  followup: ["Needs re-review", "amber"],
  archive: ["Archived", ""],
  report_ready: ["Findings ready", "blue"],
  agent_question: ["Agent question", "rose"],
};

export function ReviewRow({
  row,
  selected,
  onOpen,
  selection,
  detail,
}: {
  readonly row: MergeRequestRow;
  readonly selected: boolean;
  readonly onOpen: () => void;
  readonly selection?: { readonly checked: boolean; readonly onToggle: () => void };
  readonly detail?: ReactNode;
}) {
  const status = row.reasonKind ?? row.queueState;
  const [label, color] = reasons[status] ?? [readable(status), ""];
  const ticket = row.tickets[0];
  return (
    <button
      type="button"
      className="review-row"
      aria-current={selected || undefined}
      aria-pressed={selection?.checked}
      onClick={selection?.onToggle ?? onOpen}
    >
      <div className="row-top">
        <span className="row-identities">
          {ticket && (
            <span className="ticket-key">
              {ticket.key}
              {row.tickets.length > 1 && <small> +{row.tickets.length - 1}</small>}
            </span>
          )}
          <span className="mr-id" title={row.projectPath}>
            <Icon name="branch" size={13} />
            {row.projectPath}
            <strong>!{row.iid}</strong>
          </span>
        </span>
        {selection ? (
          <span className="row-checkbox" aria-hidden="true">
            {selection.checked && <Icon name="check" size={13} />}
          </span>
        ) : (
          <span className={`badge ${color}`}>{label}</span>
        )}
      </div>
      <strong className="row-title">{row.title}</strong>
      <div className="row-foot">
        <span className="row-person" title={row.author?.label ?? "Unknown author"}>
          <span className="avatar" aria-hidden="true">
            {row.author?.label.charAt(0).toUpperCase() ?? "?"}
          </span>
          <span className="truncate">{row.author?.label ?? "Unknown author"}</span>
          <span className="muted">Author</span>
        </span>
        <span className="actions row-facts">
          {row.draft && <span>Draft</span>}
          {row.personalApproval && (
            <span className="personal-approval">
              <Icon name="check" size={13} /> You approved
            </span>
          )}
          {row.ourComments > 0 && <span>{row.ourComments} of your comments</span>}
          {row.unreadReplies > 0 && (
            <span className="new-reply">
              {row.unreadReplies} new {row.unreadReplies === 1 ? "reply" : "replies"}
            </span>
          )}
          {detail}
        </span>
      </div>
    </button>
  );
}
