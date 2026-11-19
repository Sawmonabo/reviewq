//! Monitor ownership, frozen work and scoped result projections.

mod commands;
mod work;
pub use work::{
    CheckMember, ExecutingCheck, FollowupWorkerStore, MAX_FROZEN_OBSERVATIONS, PendingCheck,
    PreparedCheck, run_due_followups,
};

pub use commands::{
    FollowupCommands, MonitorCommand, MonitorReceipt, MonitorReplay, MonitorRequest,
    OriginalMonitorRequest, Selection, UpdatePreferences, execute_monitor_command,
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

pub enum AssessmentSelection {
    Unviewed { through: Option<String> },
    Batch(String),
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
        selection: AssessmentSelection,
        after: Option<AssessmentPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<AssessmentPage>>;
}

/// One monitored review and its exact current registration/configuration proof.
#[derive(Clone)]
pub struct MonitoredReview {
    pub review: ReviewId,
    pub run: RunId,
    pub identity: reviewq_domain::identity::MrIdentity,
    pub generation: Revision,
    pub settings: Revision,
}

/// One tracked provider review and its current optional automatic monitor.
pub struct TrackedReview {
    pub review: ReviewId,
    pub identity: reviewq_domain::identity::MrIdentity,
    pub settings: Revision,
    pub observed_before: reviewq_domain::identity::EventCursor,
    pub monitor: Option<MonitoredReview>,
    pub discovery_head: Option<reviewq_domain::identity::GitSha>,
}

pub trait FollowupObservationStore: Send + Sync {
    fn tracked_reviews(
        &self,
        after: Option<ReviewId>,
        limit: u16,
    ) -> AppFuture<'_, Vec<TrackedReview>>;
    fn record_monitor_unavailable(&self, target: MonitoredReview) -> AppFuture<'_, ()>;
    fn record_workflow_observation(
        &self,
        target: TrackedReview,
        observation: crate::sources::WorkflowSnapshot,
    ) -> AppFuture<'_, ()>;
    fn record_discovery_evidence(
        &self,
        target: TrackedReview,
        observation: Option<crate::sources::DiscoveryEvidence>,
    ) -> AppFuture<'_, ()>;
}

pub async fn poll_monitored_reviews(
    store: &dyn FollowupObservationStore,
    source: &dyn crate::sources::ReviewSourceReader,
    after: Option<ReviewId>,
) -> Result<Option<ReviewId>, crate::access::AppError> {
    const LIMIT: u16 = 8;
    let rows = store.tracked_reviews(after, LIMIT).await?;
    let next = (rows.len() == usize::from(LIMIT))
        .then(|| rows.last().map(|row| row.review.clone()))
        .flatten();
    for row in rows {
        let committed = if row.discovery_head.is_some() {
            let observation = match source.discovery_evidence(row.identity.clone()).await {
                Ok(observation) => Some(observation),
                Err(crate::access::AppError::StaleRevision) => {
                    return Err(crate::access::AppError::StaleRevision);
                }
                Err(_) => None,
            };
            store.record_discovery_evidence(row, observation).await
        } else {
            match source.workflow_snapshot(row.identity.clone()).await {
                Ok(observation) => store.record_workflow_observation(row, observation).await,
                Err(_) => match row.monitor {
                    Some(monitor) => store.record_monitor_unavailable(monitor).await,
                    None => continue,
                },
            }
        };
        if let Err(error) = committed
            && !matches!(
                error,
                crate::access::AppError::StaleRevision | crate::access::AppError::Conflict
            )
        {
            return Err(error);
        }
    }
    Ok(next)
}
