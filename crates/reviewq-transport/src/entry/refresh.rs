use std::sync::Arc;

use axum::Json;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::sources::{RefreshReceipt, RefreshState, refresh_queue};
use reviewq_protocol::queue as dto;
use reviewq_protocol::{EventCursorDto, RevisionDto, Success, UtcTimestampDto};

use super::error::ApiPath as Path;
use super::pages::{count, parse, queue_sync, source_failure, success};
use super::{ApiError, Entry, control_ops};

pub(super) async fn sync(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
) -> Result<Json<Success<dto::QueueSync>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let active = entry.active_source().await?;
    let state = entry.store.refresh_sync(actor, active.revision).await?;
    Ok(Json(success(
        queue_sync(&active, state.value),
        state.cursor,
    )?))
}

pub(super) async fn start(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    respond(
        &entry,
        &uri,
        &headers,
        "POST",
        StatusCode::ACCEPTED,
        async { project(enqueue(&entry).await?) },
    )
    .await
}

pub(super) async fn status(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    respond(&entry, &uri, &headers, "GET", StatusCode::OK, async {
        let receipt = entry
            .store
            .refresh_receipt(Actor::local_operator(), parse(&id)?)
            .await?;
        project(receipt)
    })
    .await
}

async fn respond(
    entry: &Entry,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
    method: &'static str,
    status: StatusCode,
    value: impl std::future::Future<Output = Result<dto::RefreshResponse, AppError>>,
) -> Result<Response, ApiError> {
    if headers.contains_key("x-reviewq-control") {
        let target = uri
            .path_and_query()
            .ok_or(AppError::InvalidRequest)?
            .as_str();
        let proof = entry.auth.control(method, target, headers, b"")?;
        return control_ops::signed(entry, &proof, status, value.await);
    }
    entry.auth.browser_actor(headers, method == "POST")?;
    Ok((status, Json(value.await?)).into_response())
}

pub(super) async fn enqueue(entry: &Arc<Entry>) -> Result<Snapshot<RefreshReceipt>, AppError> {
    let context = entry.active_source().await?;
    if !context.collection_enabled || context.reader.is_none() {
        return Err(AppError::Unavailable);
    }
    let store = entry.store.clone();
    tokio::spawn(async move {
        let actor = Actor::local_operator();
        let admission = store.begin_refresh(actor.clone(), context.revision).await?;
        if admission.started {
            let attempt = admission.receipt.value.attempt;
            tokio::spawn(async move {
                let _ = refresh_queue(store.as_ref(), context, actor, attempt).await;
            });
        }
        Ok(admission.receipt)
    })
    .await
    .map_err(|_| AppError::Internal)?
}

fn project(stored: Snapshot<RefreshReceipt>) -> Result<dto::RefreshResponse, AppError> {
    let value = stored.value;
    let state = match value.state {
        RefreshState::Running => dto::RefreshState::Running,
        RefreshState::Succeeded => dto::RefreshState::Succeeded,
        RefreshState::Failed => dto::RefreshState::Failed,
        RefreshState::Superseded => dto::RefreshState::Superseded,
    };
    success(
        dto::RefreshReceipt {
            refresh_id: EventCursorDto::new(value.attempt.id.get()).ok_or(AppError::Internal)?,
            settings_revision: RevisionDto::new(value.attempt.settings_revision.get())
                .ok_or(AppError::Internal)?,
            state,
            finished_at: value
                .finished_at_ms
                .map(|time| UtcTimestampDto::from_unix_milliseconds(time).ok_or(AppError::Internal))
                .transpose()?,
            total_rows: value.total_rows.map(count).transpose()?,
            error: value.error.map(source_failure),
        },
        stored.cursor,
    )
}
