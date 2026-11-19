use reviewq_app::access::{Actor, AppError};
use reviewq_app::sources::ReviewResolution;
use reviewq_domain::identity::{ReviewId, SessionKey};

use super::{Entry, pages::parse};

pub(super) enum ResolvedReview {
    Review(ReviewId, Option<SessionKey>),
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
            Ok(_) => return Ok(ResolvedReview::Review(id, None)),
            Err(AppError::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    if let Ok(key) = parse::<SessionKey>(target)
        && let Some(binding) = entry.store.registered_session(actor.clone(), key).await?
    {
        return Ok(ResolvedReview::Review(
            binding.review_id().clone(),
            Some(binding.session_key().clone()),
        ));
    }
    let active = entry.active_source().await?;
    let source = active.reader.ok_or(AppError::NotFound)?;
    let workspace = entry.store.workspace_context(actor.clone()).await?.value;
    if active.revision != workspace.revision {
        return Err(AppError::StaleRevision);
    }
    let resolved = source
        .resolve(workspace.projects, target.to_owned())
        .await?;
    if entry.active_source().await?.revision != active.revision {
        return Err(AppError::StaleRevision);
    }
    match resolved {
        ReviewResolution::Ambiguous(urls) => Ok(ResolvedReview::Alternatives(urls)),
        ReviewResolution::Found(found) => entry
            .store
            .review_by_mr(actor.clone(), found.identity)
            .await
            .map(|id| ResolvedReview::Review(id, None)),
    }
}

pub(super) fn session(
    resolved: Option<SessionKey>,
    requested: Option<&str>,
) -> Result<Option<SessionKey>, AppError> {
    let requested = requested.map(parse::<SessionKey>).transpose()?;
    if let (Some(found), Some(exact)) = (&resolved, &requested)
        && found != exact
    {
        return Err(AppError::NotFound);
    }
    Ok(resolved.or(requested))
}
