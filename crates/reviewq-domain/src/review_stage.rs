//! Visible workflow classification over observed facts.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStage {
    NeedsInput,
    Working,
    Waiting,
    Archived,
}

impl RunStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NeedsInput => "needs_input",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Archived => "archived",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stage {
    pub stage: RunStage,
    pub reason: &'static str,
}

pub struct WorkspaceEvidence<'a> {
    pub open: bool,
    pub registered: bool,
    pub newest_activity: Option<&'a str>,
    pub blocking_question: bool,
    pub any_question: bool,
    pub family_working: bool,
    pub family_failed: bool,
    pub action: Option<&'a str>,
    pub action_intent: Option<&'a str>,
    pub report_state: Option<&'a str>,
    pub followup_state: Option<&'a str>,
    pub uncertain_delivery: bool,
    pub personal_approval: Option<bool>,
    pub approval_withdrawn: bool,
    pub pending_source: bool,
    pub pending_report: bool,
    pub decision: Option<&'a str>,
    pub reviewer_comments: bool,
}

pub fn classify_review(evidence: &WorkspaceEvidence<'_>) -> Option<Stage> {
    use RunStage::{Archived, NeedsInput, Waiting, Working};
    if !evidence.registered {
        return None;
    }
    let (stage, reason) = match evidence {
        e if !e.open => (Archived, "closed_mr"),
        e if e.blocking_question => (NeedsInput, "agent_input"),
        e if e.newest_activity == Some("blocked") => (NeedsInput, "permission_required"),
        e if e.newest_activity == Some("working") => (Working, "lead_working"),
        e if e.family_failed && !e.pending_report => (NeedsInput, "agent_failed"),
        e if e.family_working => (Working, "session_working"),
        e if e.any_question => (NeedsInput, "agent_input"),
        e if matches!(e.action, Some("working" | "checking")) => (Working, "action_verifying"),
        e if e.report_state == Some("failed") => (NeedsInput, "report_failed"),
        e if e.report_state == Some("pending_completion") => (Working, "report_saving"),
        e if e.uncertain_delivery || e.followup_state == Some("reconciliation") => {
            (NeedsInput, "delivery_uncertain")
        }
        e if e.action == Some("retry_wait") => (Waiting, "action_retry"),
        e if e.action == Some("needs_input") => (NeedsInput, "action_needs_input"),
        e if e.approval_withdrawn && e.personal_approval != Some(true) => {
            (NeedsInput, "approval_withdrawn")
        }
        e if matches!(e.followup_state, Some("pending" | "reserved"))
            && e.personal_approval != Some(true) =>
        {
            (Waiting, "followup_pending")
        }
        e if e.followup_state == Some("executing") => (Working, "followup_working"),
        e if e.pending_source && e.personal_approval != Some(true) => {
            (NeedsInput, "discussion_update")
        }
        e if e.action == Some("completed") && e.action_intent == Some("comments") => {
            (Waiting, "waiting_author")
        }
        e if e.personal_approval == Some(true)
            || (e.action == Some("completed")
                && matches!(e.action_intent, Some("approval" | "both"))) =>
        {
            (Waiting, "awaiting_merge")
        }
        e if e.pending_report => (NeedsInput, "report_ready"),
        e if e.decision == Some("reviewed") => (Waiting, "awaiting_merge"),
        e if e.decision == Some("waiting_author") || e.reviewer_comments => {
            (Waiting, "waiting_author")
        }
        _ => (NeedsInput, "decision_required"),
    };
    Some(Stage { stage, reason })
}

pub fn queue_state(
    open: bool,
    approved: Option<bool>,
    pending: bool,
    changed_after_handoff: bool,
    decision: Option<&str>,
    comments: bool,
) -> &'static str {
    if !open {
        "archive"
    } else if approved == Some(true) {
        "reviewed"
    } else if pending || changed_after_handoff {
        "followup"
    } else if decision == Some("reviewed") {
        "reviewed"
    } else if decision == Some("waiting_author") || comments {
        "waiting_author"
    } else {
        "needs_review"
    }
}
