use reviewq_domain::identity::{EventCursor, MrIdentity, Revision, TicketKey};
use reviewq_domain::settings::BoardScope;

use crate::access::{Actor, AppError, AppFuture};
use crate::changes::ChangeSource;
use crate::queue::ReviewSource;
use crate::settings::ActiveSource;
use crate::workspace::{ConfiguredProject, WorkspaceStore};

mod scope;
pub use scope::{MatchedReview, ScopedIssue, ScopedMatch, ScopedSprint, SourceObservation};

/// Reads verified provider metadata outside the store transaction.
pub trait ReviewSourceReader: ChangeSource {
    fn verify_checkout(
        &self,
        project: ConfiguredProject,
        remote: String,
    ) -> AppFuture<'_, ConfiguredProject>;

    fn review(&self, identity: MrIdentity) -> AppFuture<'_, ReviewSource>;
    fn workflow_snapshot(&self, identity: MrIdentity) -> AppFuture<'_, WorkflowSnapshot>;

    fn reviews(&self, project: ConfiguredProject) -> AppFuture<'_, Vec<ReviewSource>>;
    fn ticket_reviews(
        &self,
        project: ConfiguredProject,
        keys: Vec<TicketKey>,
        open: bool,
    ) -> AppFuture<'_, Vec<MatchedReview>>;

    /// Resolves a user-supplied URL, project/IID or unique ticket in configured projects.
    fn resolve(
        &self,
        projects: Vec<ConfiguredProject>,
        target: String,
    ) -> AppFuture<'_, ReviewResolution>;
}

pub enum ReviewResolution {
    Found(ReviewSource),
    Ambiguous(Vec<String>),
}

/// A board-filtered Jira issue with provider-verified fields.
#[derive(Debug)]
pub struct ScopedTicket {
    pub key: TicketKey,
    pub title: String,
    pub project: String,
    pub status_id: u64,
    pub status: String,
    pub assignee: Option<String>,
    pub priority: Option<String>,
    pub estimate: Option<f64>,
    pub estimate_warning: Option<String>,
}

pub struct TicketPage {
    pub items: Vec<ScopedTicket>,
    pub continuation: Option<String>,
}

pub trait TicketSourceReader: Send + Sync {
    fn sprints(&self, board: u64, offset: u32) -> AppFuture<'_, scope::SprintPage>;
    fn issues(
        &self,
        board: BoardScope,
        sprint: Option<u64>,
        continuation: Option<String>,
        limit: u16,
    ) -> AppFuture<'_, TicketPage>;
}

pub use scope::SprintPage;

/// Total compact records accepted in one atomic source publication.
pub const MAX_PUBLICATION_REVIEWS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefreshAttempt {
    pub id: EventCursor,
    pub settings_revision: Revision,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SourceSync {
    pub active: bool,
    pub last_attempt_at_ms: Option<i64>,
    pub last_success_at_ms: Option<i64>,
    pub error: Option<SourceFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceFailure {
    Unavailable,
    TooLarge,
    Interrupted,
}

pub trait SourceRefreshStore: Send + Sync {
    /// Starts a refresh under the current settings revision, or reports an active attempt.
    fn begin_refresh(&self, actor: Actor, revision: Revision) -> AppFuture<'_, RefreshAttempt>;

    /// Publishes a complete scan and its success together, or records a safe failure.
    fn finish_refresh(
        &self,
        attempt: RefreshAttempt,
        result: Result<SourceObservation, SourceFailure>,
    ) -> AppFuture<'_, ()>;

    fn retained_reviews(&self, attempt: RefreshAttempt) -> AppFuture<'_, Vec<MrIdentity>>;

    /// Marks an unfinished attempt after the previous service process has exited.
    fn recover_refresh(&self, actor: Actor) -> AppFuture<'_, ()>;
}

/// Refreshes the compact queue from the currently configured projects.
pub async fn refresh_queue(
    store: &(impl WorkspaceStore + SourceRefreshStore + ?Sized),
    context: ActiveSource,
    actor: Actor,
) -> Result<usize, AppError> {
    let workspace = store.workspace_context(actor.clone()).await?.value;
    if workspace.revision != context.revision {
        return Err(AppError::StaleRevision);
    }
    let reader = context.reader.as_deref().ok_or(AppError::Unavailable)?;
    let attempt = store.begin_refresh(actor, workspace.revision).await?;
    let result = match context.tickets {
        Some(jira) => {
            scope::collect(
                store,
                reader,
                jira.as_ref(),
                &context.scope,
                workspace.projects,
                attempt,
            )
            .await
        }
        None => scan(reader, workspace.projects)
            .await
            .map(SourceObservation::validation),
    };
    let count = result
        .as_ref()
        .map(|result| result.reviews.len())
        .map_err(|error| *error);
    let observation = result.map_err(|error| match error {
        AppError::PayloadTooLarge => SourceFailure::TooLarge,
        _ => SourceFailure::Unavailable,
    });
    store.finish_refresh(attempt, observation).await?;
    count
}

async fn scan(
    source: &dyn ReviewSourceReader,
    projects: Vec<ConfiguredProject>,
) -> Result<Vec<ReviewSource>, AppError> {
    let mut reviews = Vec::new();
    for project in projects {
        let records = source.reviews(project).await?;
        if records.len() > MAX_PUBLICATION_REVIEWS - reviews.len() {
            return Err(AppError::PayloadTooLarge);
        }
        reviews.extend(records);
    }
    Ok(reviews)
}

/// One full authorized GitLab observation pinned to a particular MR head.
pub struct WorkflowSnapshot {
    pub review: ReviewSource,
    pub authenticated_actor: u64,
    pub personal_approval: Option<bool>,
    pub observed_at_ms: i64,
    pub discussions: Vec<WorkflowDiscussion>,
}

pub struct WorkflowDiscussion {
    pub id: String,
    pub root_note_id: Option<u64>,
    pub resolved_by: Option<u64>,
    pub resolved: bool,
    pub notes: Vec<WorkflowNote>,
}

pub struct WorkflowNote {
    pub id: u64,
    pub author: u64,
    pub body_hash: reviewq_domain::identity::ReportHash,
    pub positioned_head: Option<reviewq_domain::identity::GitSha>,
    pub created_at_ms: i64,
}
