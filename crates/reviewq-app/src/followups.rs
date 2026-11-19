//! Monitor ownership, frozen work and scoped result projections.

mod commands;

pub use commands::{
    FollowupCommands, MonitorCommand, MonitorReceipt, MonitorRequest, Selection, UpdatePreferences,
    execute_monitor_command,
};

use reviewq_domain::identity::{GitSha, ReceiptId, ReviewId, Revision, RunId, SubmissionId};

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;
use crate::labels::labels;
use crate::reports::FindingConclusion;

labels!(MonitorState { Enabled => "enabled", Paused => "paused", Archived => "archived" });
labels!(BatchState {
    Pending => "pending", Reserved => "reserved", Executing => "executing",
    Completed => "completed", Cancelled => "cancelled", Reconciliation => "reconciliation",
});
labels!(ResultKind { Final => "final", Unchanged => "unchanged" });
labels!(NextAction { Review => "review", WaitingAuthor => "waiting_author" });

pub const MAX_SELECTED_NOTES: u16 = 500;
pub const MAX_RESULTS: u16 = 20;
pub const MAX_ASSESSMENTS: u16 = 500;

pub struct Preferences {
    pub automatic: bool,
    pub concurrency: u8,
    pub revision: Revision,
}

pub struct Monitor {
    pub run: Option<RunId>,
    pub generation: Revision,
    pub state: MonitorState,
    pub policy_override: Option<bool>,
    pub binding_verified: bool,
    pub reason: Option<String>,
    pub updated_at_ms: i64,
    pub selections: Vec<SelectedThread>,
}

pub struct SelectedThread {
    pub original_note_id: String,
    pub discussion_id: Option<String>,
    pub reviewer_id: Option<String>,
    pub selected: bool,
    pub version: Option<String>,
    pub resolved: Option<bool>,
    pub finding_ids: Vec<String>,
}

pub struct Batch {
    pub id: String,
    pub run: Option<RunId>,
    pub generation: Revision,
    pub state: BatchState,
    pub member_count: u64,
    pub verified: bool,
    pub source_sha: Option<GitSha>,
    pub target_sha: Option<GitSha>,
    pub submission: Option<SubmissionId>,
    pub hold_reason: Option<String>,
    pub due_at_ms: Option<i64>,
    pub first_at_ms: i64,
    pub latest_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

pub struct ResultSummary {
    pub batch_id: String,
    pub run: RunId,
    pub report: Option<ReceiptId>,
    pub kind: ResultKind,
    pub checked_sha: Option<GitSha>,
    pub completed_at_ms: i64,
}

pub struct Enrollment {
    pub preferences: Preferences,
    pub monitor: Option<Monitor>,
    pub batches: Vec<Batch>,
    pub latest: Option<ResultSummary>,
    pub unviewed_batches: u64,
}

#[derive(Clone)]
pub struct ResultPosition {
    pub completed_at_ms: i64,
    pub batch_id: String,
}

pub struct ResultPage {
    pub items: Vec<ResultSummary>,
    pub has_more: bool,
}

pub struct Assessment {
    pub batch_id: String,
    pub receipt_id: Option<ReceiptId>,
    pub discussion_id: String,
    pub conclusion: FindingConclusion,
    pub reason: String,
    pub next_action: NextAction,
    pub finding_ids: Vec<String>,
    pub ordinal: i64,
}

#[derive(Clone)]
pub struct AssessmentPosition {
    pub through_batch_id: String,
    pub ordinal: i64,
    pub discussion_id: String,
}

pub struct AssessmentPage {
    pub items: Vec<Assessment>,
    pub through_batch_id: Option<String>,
    pub has_more: bool,
}

pub trait FollowupStore: Send + Sync {
    fn read_followup_preferences(&self, actor: Actor) -> AppFuture<'_, Snapshot<Preferences>>;
    fn read_followups(&self, actor: Actor, review: ReviewId)
    -> AppFuture<'_, Snapshot<Enrollment>>;
    fn read_followup_results(
        &self,
        actor: Actor,
        review: ReviewId,
        after: Option<ResultPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ResultPage>>;
    fn read_followup_assessments(
        &self,
        actor: Actor,
        review: ReviewId,
        through: Option<String>,
        after: Option<AssessmentPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<AssessmentPage>>;
}
