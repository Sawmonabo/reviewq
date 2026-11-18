use crate::access::{Actor, AppError, AppFuture};
use crate::queue::{QueueStore, ReviewSource};
use crate::workspace::{ConfiguredProject, WorkspaceStore};

/// Reads verified provider metadata outside the store transaction.
pub trait ReviewSourceReader: Send + Sync {
    fn reviews(&self, project: ConfiguredProject) -> AppFuture<'_, Vec<ReviewSource>>;
}

/// Total compact records accepted in one atomic source publication.
pub const MAX_PUBLICATION_REVIEWS: usize = 4096;

/// Refreshes the compact queue from the currently configured projects.
pub async fn refresh_queue(
    store: &(impl WorkspaceStore + QueueStore + ?Sized),
    source: &dyn ReviewSourceReader,
    actor: Actor,
) -> Result<usize, AppError> {
    let workspace = store.workspace_context(actor.clone()).await?.value;
    let mut reviews = Vec::new();
    for project in workspace.projects {
        let records = source.reviews(project).await?;
        if records.len() > MAX_PUBLICATION_REVIEWS - reviews.len() {
            return Err(AppError::PayloadTooLarge);
        }
        reviews.extend(records);
    }
    let count = reviews.len();
    store
        .publish_reviews(actor, workspace.revision, reviews)
        .await?;
    Ok(count)
}
