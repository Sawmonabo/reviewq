//! Local decisions and captured, independently handled review evidence.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reviewq_domain::identity::{
    GitSha, NotificationId, ReceiptId, ReviewId, Revision, RunId, SessionKey, SubmissionId,
};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::labels::labels;
use crate::notifications::Kind;
use crate::queue::{MergeRequestState, ReviewSource};
use crate::sources::ReviewSourceReader;

labels!(Disposition {
    Reviewed => "reviewed",
    WaitingAuthor => "waiting_author",
    NeedsReview => "needs_review",
});

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decision {
    pub revision: Revision,
    pub disposition: Option<Disposition>,
    pub reviewed_sha: Option<GitSha>,
    pub reviewed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportFrontier {
    pub run: RunId,
    pub receipt: ReceiptId,
    pub attention: ReceiptId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedAttention {
    pub ordinal: i64,
    pub id: NotificationId,
    pub kind: Kind,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionFrontier {
    pub run: RunId,
    pub session: SessionKey,
    pub event: ObservedAttention,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Frontier {
    pub source: Vec<ObservedAttention>,
    pub reports: Vec<ReportFrontier>,
    pub sessions: Vec<SessionFrontier>,
}

#[derive(Clone)]
pub struct DecisionRequest {
    pub review: ReviewId,
    pub submission: SubmissionId,
    pub expected_revision: Revision,
    pub expected_settings: Revision,
    pub source_sha: GitSha,
    pub checkpoint_id: Option<String>,
    pub disposition: Option<Disposition>,
    pub terminal_target: Option<String>,
}

pub struct TerminalDecision {
    pub target: String,
    pub submission: SubmissionId,
    pub source_sha: GitSha,
    pub disposition: Disposition,
}

pub struct DecisionResult {
    pub review: ReviewId,
    pub decision: Decision,
    pub checkpoint_id: Option<String>,
}

pub trait DecisionStore: Send + Sync {
    fn terminal_decision_replay(
        &self,
        actor: Actor,
        input: TerminalDecision,
    ) -> AppFuture<'_, Option<Snapshot<DecisionResult>>>;

    fn read_decision_replay(
        &self,
        actor: Actor,
        input: DecisionRequest,
    ) -> AppFuture<'_, Option<Snapshot<DecisionResult>>>;
    fn decision_source(&self, actor: Actor, review: ReviewId) -> AppFuture<'_, ReviewSource>;
    fn commit_decision(
        &self,
        actor: Actor,
        input: DecisionRequest,
        prior: ReviewSource,
        observed: ReviewSource,
        checkpoint: Option<Frontier>,
    ) -> AppFuture<'_, Snapshot<DecisionResult>>;
}

#[derive(Default)]
pub struct Checkpoints {
    entries: Mutex<HashMap<String, Captured>>,
}

struct Captured {
    actor: Actor,
    review: ReviewId,
    settings: Revision,
    frontier: Frontier,
    issued: Instant,
}

const RETAINED_CHECKPOINTS: usize = 1024;
const MAX_AGE: Duration = Duration::from_secs(30 * 60);

impl Checkpoints {
    pub fn remember(
        &self,
        id: String,
        actor: Actor,
        review: ReviewId,
        settings: Revision,
        frontier: Frontier,
    ) -> Result<(), AppError> {
        let mut book = self.entries.lock().map_err(|_| AppError::Internal)?;
        book.retain(|_, entry| entry.issued.elapsed() <= MAX_AGE);
        if book.len() >= RETAINED_CHECKPOINTS {
            let oldest = book
                .iter()
                .min_by_key(|(_, entry)| entry.issued)
                .map(|(id, _)| id.clone())
                .ok_or(AppError::Internal)?;
            book.remove(&oldest);
        }
        book.insert(
            id,
            Captured {
                actor,
                review,
                settings,
                frontier,
                issued: Instant::now(),
            },
        );
        Ok(())
    }

    fn resolve(
        &self,
        id: &str,
        actor: &Actor,
        input: &DecisionRequest,
    ) -> Result<Frontier, AppError> {
        let book = self.entries.lock().map_err(|_| AppError::Internal)?;
        let captured = book
            .get(id)
            .filter(|value| {
                value.issued.elapsed() <= MAX_AGE
                    && value.actor == *actor
                    && value.review == input.review
                    && value.settings == input.expected_settings
            })
            .ok_or(AppError::Conflict)?;
        Ok(captured.frontier.clone())
    }
}

pub async fn execute_decision(
    store: &(impl DecisionStore + ?Sized),
    source: Option<&dyn ReviewSourceReader>,
    checkpoints: &Checkpoints,
    actor: Actor,
    input: DecisionRequest,
) -> Result<Snapshot<DecisionResult>, AppError> {
    if input.disposition.is_none() && input.checkpoint_id.is_none() {
        return Err(AppError::InvalidRequest);
    }
    if let Some(previous) = store
        .read_decision_replay(actor.clone(), input.clone())
        .await?
    {
        return Ok(previous);
    }
    let checkpoint = input
        .checkpoint_id
        .as_deref()
        .map(|id| checkpoints.resolve(id, &actor, &input))
        .transpose()?;
    let prior = store
        .decision_source(actor.clone(), input.review.clone())
        .await?;
    let current = source
        .ok_or(AppError::Unavailable)?
        .review(prior.identity.clone())
        .await?;
    if current.identity != prior.identity || current.head_sha != input.source_sha {
        return Err(AppError::StaleRevision);
    }
    if current.state != MergeRequestState::Open {
        return Err(AppError::Conflict);
    }
    store
        .commit_decision(actor, input, prior, current, checkpoint)
        .await
}
