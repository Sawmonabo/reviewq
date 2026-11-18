//! Verified review observations and scoped selection reads.

use reviewq_domain::identity::{GitSha, MrIdentity, ReviewId, Revision, RunId, SessionKey};
use reviewq_domain::session::{HostProvider, LeadTarget};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;

/// Current provider evidence used to create the compact registered review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSource {
    pub identity: MrIdentity,
    pub project_path: String,
    pub title: String,
    pub head_sha: GitSha,
    pub state: MergeRequestState,
    pub updated_at_ms: i64,
}

/// Authoritative provider lifecycle of a merge request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MergeRequestState {
    Open,
    Closed,
    Merged,
}

/// One scoped review and its requested or default registered lead.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedLead {
    review_id: ReviewId,
    lead: Option<LeadTarget>,
}

impl SelectedLead {
    pub fn new(review_id: ReviewId, lead: Option<LeadTarget>) -> Result<Self, AppError> {
        if lead
            .as_ref()
            .is_some_and(|target| target.review_id() != &review_id)
        {
            return Err(AppError::NotFound);
        }
        Ok(Self { review_id, lead })
    }

    pub fn review_id(&self) -> &ReviewId {
        &self.review_id
    }

    pub fn lead(&self) -> Option<&LeadTarget> {
        self.lead.as_ref()
    }
}

/// Direct lookup used by review bookmarks and the selected-run browser path.
pub trait ReviewLookup: Send + Sync {
    /// An explicit missing or foreign run always returns scoped not-found.
    fn resolve_selection(
        &self,
        actor: Actor,
        review_id: ReviewId,
        run_id: Option<RunId>,
    ) -> AppFuture<'_, Snapshot<SelectedLead>>;
}

/// Maximum rows in one compact queue, overview, or run-detail read.
pub const MAX_QUEUE_PAGE: u16 = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PageContext {
    pub generation: Revision,
    pub authority: [u8; 32],
    pub limit: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuePosition {
    pub context: PageContext,
    pub updated_at_ms: i64,
    pub review_id: ReviewId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueItem {
    pub review_id: ReviewId,
    pub source: ReviewSource,
    pub registered_lead_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuePage {
    pub items: Vec<QueueItem>,
    pub next: Option<QueuePosition>,
    pub total: u64,
    pub settings_revision: Revision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunPosition {
    pub context: PageContext,
    pub review_id: ReviewId,
    pub started_at_ms: i64,
    pub run_id: RunId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSummary {
    pub run_id: RunId,
    pub started_at_ms: i64,
    pub lead_session_key: SessionKey,
    pub provider: HostProvider,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewOverview {
    pub item: QueueItem,
    pub settings_revision: Revision,
    pub runs: Vec<RunSummary>,
    pub next_run: Option<RunPosition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSummary {
    pub session_key: SessionKey,
    pub parent: Option<SessionKey>,
    pub provider: HostProvider,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPosition {
    pub context: PageContext,
    pub run_id: RunId,
    pub session_key: SessionKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunDetail {
    pub lead: LeadTarget,
    pub sessions: Vec<SessionSummary>,
    pub next_session: Option<SessionPosition>,
}

pub trait QueueStore: Send + Sync {
    /// Persists verified provider observations after provider I/O has completed.
    fn observe_reviews(
        &self,
        actor: Actor,
        source: Vec<ReviewSource>,
    ) -> AppFuture<'_, Snapshot<Vec<ReviewId>>>;

    /// Publishes a complete bounded source scan under the current settings revision.
    fn publish_reviews(
        &self,
        actor: Actor,
        expected_settings: Revision,
        source: Vec<ReviewSource>,
    ) -> AppFuture<'_, Snapshot<()>>;

    fn read_queue(
        &self,
        actor: Actor,
        after: Option<QueuePosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<QueuePage>>;

    fn read_review(
        &self,
        actor: Actor,
        review_id: ReviewId,
        after: Option<RunPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ReviewOverview>>;

    fn read_run(
        &self,
        actor: Actor,
        run_id: RunId,
        after: Option<SessionPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<RunDetail>>;
}
