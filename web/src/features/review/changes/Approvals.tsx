import { useState } from "react";
import { readApprovals } from "../../../api/changes";
import type { ApprovalView } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { formatTime } from "../../../lib/presentation";

export function Approvals({
  reviewId,
  displayedSha,
}: {
  readonly reviewId: string;
  readonly displayedSha: string | undefined;
}) {
  const [open, setOpen] = useState(false);
  const approvals = useResource(
    `approvals:${reviewId}`,
    (signal) => readApprovals(reviewId, signal),
    [{ kind: "review", id: reviewId }],
  );
  const value = approvals.data;
  const status = approvalStatus(value);
  const stale = Boolean(
    value && (approvals.error || (displayedSha && value.sourceSha !== displayedSha)),
  );
  const label = approvals.pending ? "Checking…" : status.label;

  return (
    <details className="approval-card" onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary
        className={`button-label approval-summary ${status.met && !stale ? "approval-met" : ""}`}
      >
        <Icon name="check" size={14} />
        <span>
          Approvals {label}
          {stale && " · Stale"}
        </span>
        <Icon name="chevron" size={11} />
      </summary>
      {open && (
        <div className="stack approval-popup">
          <h3>Approval requirements</h3>
          {approvals.error && <Notice retry={approvals.retry}>{approvals.error}</Notice>}
          {approvals.pending && <Loading label="Loading approval requirements…" />}
          {stale && (
            <p className="meta">Status may differ from the requirements for the displayed patch.</p>
          )}
          {value && <ApprovalDetail value={value} />}
        </div>
      )}
    </details>
  );
}

function approvalStatus(value: ApprovalView | undefined): { label: string; met: boolean } {
  const required = value?.approvals?.required;
  const left = value?.approvals?.left;
  const rules = value?.rules?.filter((rule) => rule.required > 0);
  if (required === 0 || (required == null && rules?.length === 0)) {
    return { label: "Not required", met: true };
  }
  if (left != null && left > 0) {
    return { label: `${left} remaining`, met: false };
  }
  if (
    value?.approvals?.approved ||
    (required != null && left === 0) ||
    (rules?.length && rules.every((rule) => rule.approved))
  ) {
    return { label: "Met", met: true };
  }
  if (required != null) return { label: `${required} required`, met: false };
  return { label: rules?.length ? "Required" : "Unavailable", met: false };
}

function ApprovalDetail({ value }: { readonly value: ApprovalView }) {
  return (
    <>
      <p className="meta">
        Last checked {formatTime(value.observedAt)} · {value.sourceSha.slice(0, 12)}
      </p>
      <p>
        {value.personalApproval === null
          ? "Your approval is unknown"
          : value.personalApproval
            ? "You approved this MR"
            : "You have not approved this MR"}
      </p>
      {value.approvals?.left != null && value.approvals.left > 0 && (
        <p>{value.approvals.left} more approvals needed</p>
      )}
      {value.approvals?.approvers.map((approver) => (
        <span key={approver.id}>
          {approver.username ?? approver.id}
          {approver.approvedAt && ` · ${formatTime(approver.approvedAt)}`}
        </span>
      ))}
      {value.rules?.map((rule) => (
        <div className="spread" key={rule.id}>
          <span>{rule.name}</span>
          <span className={rule.approved ? "approval-met" : "meta"}>
            {rule.required ? `${rule.required} required` : "Optional"}
            {rule.approved && " · Satisfied"}
          </span>
        </div>
      ))}
      {value.warnings.map((warning) => (
        <p className="meta" key={warning}>
          {warning === "approvals_unavailable"
            ? "Approval state unavailable"
            : "Approval rules unavailable"}
        </p>
      ))}
    </>
  );
}
