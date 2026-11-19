import { useId, useRef, useState } from "react";
import { readApprovals } from "../../../api/changes";
import type { ApprovalView } from "../../../api/generated/contracts";
import { Icon } from "../../../components/Icon";
import { Loading, Notice } from "../../../components/PanelState";
import { useResource } from "../../../hooks/useResource";
import { formatActivityTime, safeUrl } from "../../../lib/presentation";

export function Approvals({
  reviewId,
  displayedSha,
  webUrl,
}: {
  readonly reviewId: string;
  readonly displayedSha: string | undefined;
  readonly webUrl: string;
}) {
  const [open, setOpen] = useState(false);
  const popoverId = useId();
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popupRef = useRef<HTMLElement>(null);
  const source = safeUrl(webUrl);
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
    <div
      className="approval-card"
      onBlur={(event) => {
        const popup = popupRef.current;
        if (
          event.relatedTarget &&
          !event.currentTarget.contains(event.relatedTarget) &&
          popup?.matches(":popover-open")
        )
          popup.hidePopover();
      }}
    >
      <button
        ref={triggerRef}
        type="button"
        className={`button-label approval-summary ${status.met && !stale ? "approval-met" : ""}`}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={`Approvals: ${label}${stale ? " · Stale" : ""}`}
        popoverTarget={popoverId}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown" && !open) {
            event.preventDefault();
            event.currentTarget.click();
          }
        }}
      >
        {status.met && status.label !== "Not required" && <Icon name="check" size={14} />}
        <span>
          Approvals {label}
          {stale && " · Stale"}
        </span>
        <Icon name="chevron" size={11} />
      </button>
      <section
        id={popoverId}
        ref={popupRef}
        role="dialog"
        aria-label="Approval requirements"
        tabIndex={-1}
        popover="auto"
        className="stack approval-popup"
        onToggle={(event) => {
          const shown = event.newState === "open";
          setOpen(shown);
          if (shown) event.currentTarget.focus({ preventScroll: true });
        }}
        onKeyDown={(event) => {
          if (event.key !== "Escape") return;
          event.preventDefault();
          event.stopPropagation();
          event.currentTarget.hidePopover();
          triggerRef.current?.focus({ preventScroll: true });
        }}
      >
        {open && (
          <>
            <h3>Approval requirements</h3>
            {approvals.error && <Notice retry={approvals.retry}>{approvals.error}</Notice>}
            {approvals.pending && <Loading label="Loading approval requirements…" />}
            {stale && (
              <p className="meta">
                Status may differ from the requirements for the displayed patch.
              </p>
            )}
            {(stale || status.label === "Unavailable") && source && (
              <a href={source} target="_blank" rel="noreferrer">
                Check GitLab ↗
              </a>
            )}
            {value && <ApprovalDetail value={value} source={source} stale={stale} />}
          </>
        )}
      </section>
    </div>
  );
}

function approvalStatus(value: ApprovalView | undefined): { label: string; met: boolean } {
  const required = value?.approvals?.required;
  const left = value?.approvals?.left;
  const rules = value?.rules?.filter((rule) => rule.required > 0);
  if (rules?.length === 1) {
    const rule = rules[0];
    if (rule) return { label: ruleProgress(rule, value), met: rule.approved };
  }
  if (rules?.length) {
    const met = rules.every((rule) => rule.approved);
    return {
      label: met
        ? "Met"
        : `${rules.filter((rule) => rule.approved).length} of ${rules.length} rules`,
      met,
    };
  }
  if ((required === 0 || (required == null && rules?.length === 0)) && !left)
    return { label: "Not required", met: true };
  if (required != null && left != null && left <= required)
    return { label: `${required - left} of ${required}`, met: left === 0 };
  if (left != null && left > 0) return { label: `${left} remaining`, met: false };
  if (required != null) return { label: `${required} required`, met: false };
  return { label: "Unavailable", met: false };
}

function ruleProgress(
  rule: NonNullable<ApprovalView["rules"]>[number],
  value: ApprovalView | undefined,
) {
  if (!rule.required) return "Optional";
  const count = rule.approvedCount;
  if (count != null) return `${Math.min(count, rule.required)} of ${rule.required}`;
  const left = value?.approvals?.left;
  if (
    value?.rules?.filter((item) => item.required > 0).length === 1 &&
    value.approvals?.required === rule.required &&
    left != null
  )
    return `${Math.max(0, rule.required - left)} of ${rule.required}`;
  if (rule.approved) return `${rule.required} of ${rule.required}`;
  return `${rule.required} required`;
}

function ApprovalDetail({
  value,
  source,
  stale,
}: {
  readonly value: ApprovalView;
  readonly source: string | undefined;
  readonly stale: boolean;
}) {
  return (
    <>
      <p className="meta">
        Last checked {formatActivityTime(value.observedAt)} · {value.sourceSha.slice(0, 12)}
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
      {value.rules?.map((rule) => (
        <div className="spread" key={rule.id}>
          <span>{rule.name}</span>
          <span className={rule.approved && !stale ? "approval-met button-label" : "meta"}>
            {ruleProgress(rule, value)}
            {rule.required > 0 && rule.approved && !stale && <Icon name="check" size={12} />}
          </span>
        </div>
      ))}
      {value.approvals && (
        <div className="approval-people" aria-label="Approved by">
          {!value.approvals.approvers.length && <p className="meta">No approvals yet.</p>}
          {value.approvals.approvers.map((approver) => {
            const label = approver.name || approver.username || approver.id;
            const name = approver.isYou === true ? "You" : label;
            const href =
              source && approver.username && /^[\w][\w.-]*$/.test(approver.username)
                ? safeUrl(`/${encodeURIComponent(approver.username)}`, source)
                : undefined;
            const title = [
              label,
              approver.username && `@${approver.username}`,
              approver.approvedAt && `Approved ${formatActivityTime(approver.approvedAt)}`,
            ]
              .filter(Boolean)
              .join(" · ");
            const content = (
              <>
                <span className="approval-avatar" aria-hidden="true">
                  {label
                    .split(/\s+/)
                    .slice(0, 2)
                    .map((part) => part.charAt(0))
                    .join("")
                    .toUpperCase()}
                </span>
                {name}
                {!stale && <Icon name="check" size={12} />}
              </>
            );
            return href ? (
              <a
                className="approval-person"
                key={approver.id}
                href={href}
                target="_blank"
                rel="noreferrer"
                title={title}
              >
                {content}
              </a>
            ) : (
              <span className="approval-person" key={approver.id} title={title}>
                {content}
              </span>
            );
          })}
        </div>
      )}
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
