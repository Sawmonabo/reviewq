//! Verified review observations and scoped selection reads.

use reviewq_domain::identity::{
    GitSha, MrIdentity, ReviewId, Revision, RunId, SessionKey, TicketKey,
};
use reviewq_domain::session::{HostProvider, LeadTarget, SessionTarget};

use crate::access::{Actor, AppError, AppFuture};
use crate::decisions::{Decision, Frontier};
use crate::events::Snapshot;
use crate::sources::SourceSync;

mod tracking;
mod workspace;
pub use tracking::{TicketAuthority, TrackReview, TrackedReviewReceipt, TrackingStore};
pub use workspace::{
    RunCard, RunCards, RunCardsFilter, RunCardsPosition, StageCounts, WorkspaceQueue,
};

/// Current provider evidence used to create the compact registered review.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSource {
    pub identity: MrIdentity,
    pub project_path: String,
    pub title: String,
    pub head_sha: GitSha,
    pub state: MergeRequestState,
    pub updated_at_ms: i64,
    pub author: Option<ReviewAuthor>,
    pub draft: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAuthor {
    pub id: u64,
    pub username: String,
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
    fn review_by_mr(&self, actor: Actor, mr: MrIdentity) -> AppFuture<'_, ReviewId>;

    fn authorized_source(
        &self,
        actor: Actor,
        review: ReviewId,
    ) -> AppFuture<'_, Snapshot<(Revision, MrIdentity)>>;

    fn registered_session(
        &self,
        actor: Actor,
        session: SessionKey,
    ) -> AppFuture<'_, Option<SessionTarget>>;

    fn run_for_session(
        &self,
        actor: Actor,
        review: ReviewId,
        session: SessionKey,
    ) -> AppFuture<'_, RunId>;

    fn management_reviews(
        &self,
        actor: Actor,
        filter: ManagementFilter,
        offset: u32,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ManagementReviews>>;

    fn management_runs(
        &self,
        actor: Actor,
        review: ReviewId,
        offset: u32,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ManagementRuns>>;

    /// An explicit missing or foreign run always returns scoped not-found.
    fn resolve_selection(
        &self,
        actor: Actor,
        review_id: ReviewId,
        run_id: Option<RunId>,
    ) -> AppFuture<'_, Snapshot<SelectedLead>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementFilter {
    All,
    Unstarted,
    Archived,
    NeedsInput,
    Working,
    Waiting,
    AgentQuestions,
}

#[derive(Debug)]
pub struct ManagementReviews {
    pub items: Vec<QueueItem>,
    pub total: u64,
    pub sync: SourceSync,
}

#[derive(Debug)]
pub struct ManagementRuns {
    pub item: QueueItem,
    pub runs: Vec<RunSummary>,
    pub total: u64,
    pub sync: SourceSync,
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
    pub key: QueueKey,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueKey {
    Review {
        updated_at_ms: i64,
        review_id: ReviewId,
    },
    Ticket(TicketKey),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompactTicket {
    pub key: TicketKey,
    pub title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnmatchedTicket {
    pub ticket: CompactTicket,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QueueListItem {
    Review(Box<QueueItem>),
    Unmatched(UnmatchedTicket),
}

impl QueueListItem {
    pub fn key(&self) -> QueueKey {
        match self {
            Self::Review(item) => QueueKey::Review {
                updated_at_ms: item.source.updated_at_ms,
                review_id: item.review_id.clone(),
            },
            Self::Unmatched(item) => QueueKey::Ticket(item.ticket.key.clone()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueItem {
    pub review_id: ReviewId,
    pub source: ReviewSource,
    pub tickets: Vec<CompactTicket>,
    pub registered_lead_count: u64,
    pub decision: Decision,
    pub pending_source_updates: bool,
    pub pending_native_attention: bool,
    pub pending_report_attention: bool,
    pub signals: ReviewSignals,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewSignals {
    pub own: bool,
    pub personal_approval: Option<bool>,
    pub our_comments: u64,
    pub unread_replies: u64,
    pub has_questions: bool,
    pub stage_reason: Option<String>,
    pub queue_state: String,
    pub run: Option<RunId>,
    pub focus: Option<SessionKey>,
    pub full_mr_reviewed_sha: Option<GitSha>,
    pub coverage: Option<String>,
    pub last_activity_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueuePage {
    pub items: Vec<QueueListItem>,
    pub next: Option<QueuePosition>,
    pub total: u64,
    pub total_mrs: u64,
    pub settings_revision: Revision,
    pub sync: SourceSync,
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
    pub frontier: Frontier,
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
