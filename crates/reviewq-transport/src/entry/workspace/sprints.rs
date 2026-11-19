use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::queue::{SprintPair, collect_sprint_pairs};
use reviewq_domain::identity::Revision;
use reviewq_protocol::queue as dto;
use reviewq_protocol::{RevisionDto, UpstreamIdDto};

use crate::entry::error::ApiBytes;
use crate::entry::pages::success;
use crate::entry::{ApiError, Entry};

pub(in crate::entry) async fn load(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Json<dto::LoadSprintsResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let query: dto::LoadSprintsRequest =
        serde_json::from_slice(&body).map_err(|_| AppError::InvalidRequest)?;
    if query.pairs.is_empty() || query.pairs.len() > 32 {
        return Err(AppError::InvalidRequest.into());
    }
    let expected =
        Revision::try_from(query.expected_revision.get()).map_err(|_| AppError::InvalidRequest)?;
    let _permit =
        tokio::time::timeout(std::time::Duration::from_secs(30), entry.sprint_load.lock())
            .await
            .map_err(|_| AppError::Busy)?;
    let active = entry.active_source().await?;
    if active.revision != expected {
        return Err(AppError::StaleRevision.into());
    }
    let pairs = query
        .pairs
        .into_iter()
        .map(|pair| SprintPair {
            board: pair.board_id.get(),
            sprint: pair.sprint_id.get(),
        })
        .collect();
    let missing = entry
        .store
        .unloaded_sprints(actor.clone(), expected, pairs)
        .await?
        .value;
    let loaded = if missing.is_empty() {
        Vec::new()
    } else {
        let source = active.tickets.ok_or(AppError::Unavailable)?;
        collect_sprint_pairs(source.as_ref(), &active.scope, missing).await?
    };
    let saved = entry.store.publish_sprints(actor, expected, loaded).await?;
    let loaded_pairs = saved
        .value
        .into_iter()
        .map(project)
        .collect::<Result<_, _>>()?;
    Ok(Json(success(
        dto::LoadedSprints {
            settings_revision: RevisionDto::new(expected.get()).ok_or(AppError::Internal)?,
            loaded_pairs,
        },
        saved.cursor,
    )?))
}

pub(super) fn project(pair: SprintPair) -> Result<dto::SprintPair, AppError> {
    Ok(dto::SprintPair {
        board_id: UpstreamIdDto::new(pair.board).ok_or(AppError::Internal)?,
        sprint_id: UpstreamIdDto::new(pair.sprint).ok_or(AppError::Internal)?,
    })
}
