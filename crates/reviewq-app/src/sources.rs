use reviewq_domain::identity::{EventCursor, MrIdentity, Revision};

use crate::access::{Actor, AppError, AppFuture};
use crate::changes::ChangeSource;
use crate::queue::ReviewSource;
use crate::workspace::{ConfiguredProject, WorkspaceStore};

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
        result: Result<Vec<ReviewSource>, SourceFailure>,
    ) -> AppFuture<'_, ()>;

    /// Marks an unfinished attempt after the previous service process has exited.
    fn recover_refresh(&self, actor: Actor) -> AppFuture<'_, ()>;
}

/// Refreshes the compact queue from the currently configured projects.
pub async fn refresh_queue(
    store: &(impl WorkspaceStore + SourceRefreshStore + ?Sized),
    source: &dyn ReviewSourceReader,
    actor: Actor,
) -> Result<usize, AppError> {
    let workspace = store.workspace_context(actor.clone()).await?.value;
    let attempt = store.begin_refresh(actor, workspace.revision).await?;
    let result = scan(source, workspace.projects).await;
    let count = result.as_ref().map(Vec::len).map_err(|error| *error);
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
    pub personal_approval: bool,
    pub observed_at_ms: i64,
    pub discussions: Vec<WorkflowDiscussion>,
}

pub struct WorkflowDiscussion {
    pub id: String,
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
