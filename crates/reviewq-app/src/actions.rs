//! Exact native publication authority and independently observed target evidence.

mod publication;

pub use publication::{
    ActionCommandStore, ActionDeclaration, ActionWork, CommentTarget, PublicationEvidence,
    TargetObservation, confirms_prior_target, declare_publication, reconcile_publication,
};

use reviewq_domain::identity::{
    ActionId, GitSha, NativeInputId, NativeTurnId, ReportHash, Revision, RunId, SubmissionId,
};

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;
use crate::labels::labels;

labels!(Intent {
    Comments => "comments", Approval => "approval", Both => "both", Cancel => "cancel",
});
labels!(State {
    Working => "working", Checking => "checking", Completed => "completed",
    RetryWait => "retry_wait", NeedsInput => "needs_input", Cancelled => "cancelled",
});
labels!(Evidence {
    Unknown => "unknown", Absent => "absent", Confirmed => "confirmed",
});
labels!(Approval {
    NotRequested => "not_requested", Unknown => "unknown", Absent => "absent", Confirmed => "confirmed",
});

pub struct Action {
    pub id: ActionId,
    pub run: RunId,
    pub request_entry: Option<NativeInputId>,
    pub turn: Option<NativeTurnId>,
    pub call_id: Option<String>,
    pub reviewed_sha: Option<GitSha>,
    pub authority_verified: bool,
    pub revision: Revision,
    pub intent: Intent,
    pub state: State,
    pub targets: Vec<Target>,
    pub approval: Approval,
    pub approval_observed_at_ms: Option<i64>,
    pub checked_at_ms: Option<i64>,
    pub attempts: u8,
    pub pending_submission: Option<SubmissionId>,
    pub last_executed_submission: Option<SubmissionId>,
    pub last_executed_turn: Option<NativeTurnId>,
    pub next_check_at_ms: Option<i64>,
    pub reason: Option<String>,
}

pub struct Target {
    pub body_sha256: ReportHash,
    pub discussion_id: Option<String>,
    pub evidence: Evidence,
    pub note_id: Option<String>,
    pub observed_at_ms: Option<i64>,
}

pub trait ActionStore: Send + Sync {
    fn read_action(&self, actor: Actor, run: RunId) -> AppFuture<'_, Snapshot<Option<Action>>>;
}
