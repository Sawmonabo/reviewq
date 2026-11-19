use reviewq_app::access::{Actor, AppError};
use reviewq_app::sources::ReviewResolution;
use reviewq_domain::identity::ReviewId;

use super::{Entry, pages::parse};

pub(super) enum ResolvedReview {
    Review(ReviewId),
    Alternatives(Vec<String>),
}

pub(super) async fn resolve(
    entry: &Entry,
    actor: &Actor,
    target: &str,
) -> Result<ResolvedReview, AppError> {
    if let Ok(id) = parse::<ReviewId>(target) {
        match entry
            .store
            .resolve_selection(actor.clone(), id.clone(), None)
            .await
        {
            Ok(_) => return Ok(ResolvedReview::Review(id)),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    let source = entry
        .active_source()
        .await?
        .reader
        .ok_or(AppError::NotFound)?;
    let workspace = entry.store.workspace_context(actor.clone()).await?.value;
    match source
        .resolve(workspace.projects, target.to_owned())
        .await?
    {
        ReviewResolution::Ambiguous(urls) => Ok(ResolvedReview::Alternatives(urls)),
        ReviewResolution::Found(found) => entry
            .store
            .review_by_mr(actor.clone(), found.identity)
            .await
            .map(ResolvedReview::Review),
    }
}
