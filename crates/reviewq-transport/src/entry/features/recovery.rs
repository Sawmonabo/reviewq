use std::sync::Arc;

use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router, extract::State};
use reviewq_app::access::AppError;
use reviewq_app::hosts::recovery as app;
use reviewq_protocol::{HistoryTurnIdDto, ResourceIdDto, RevisionDto, recovery as dto};
use serde::de::DeserializeOwned;

use crate::entry::error::{ApiBytes, ApiPath as Path};
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

pub(super) fn routes() -> Router<Arc<Entry>> {
    Router::new()
        .route("/settings/agents/recovery", get(preferences).put(save))
        .route("/runs/{run}/sessions/{session}/recovery", get(current))
        .route(
            "/runs/{run}/sessions/{session}/recovery/cancel",
            post(cancel),
        )
        .route("/runs/{run}/sessions/{session}/interrupt", post(interrupt))
}

async fn preferences(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
) -> Result<Json<dto::RecoveryPreferencesResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let stored = entry.store.recovery_preferences(actor).await?;
    Ok(Json(success(project(stored.value)?, stored.cursor)?))
}

async fn save(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::RecoveryPreferencesResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::SavePreferences = read(&bytes)?;
    let stored = entry
        .store
        .save_recovery_preferences(
            actor,
            reviewq_domain::identity::Revision::try_from(input.expected_revision.get())
                .map_err(|_| AppError::InvalidRequest)?,
            input.auto_resume_codex_reviews,
        )
        .await?;
    Ok(Json(success(project(stored.value)?, stored.cursor)?))
}

async fn current(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
) -> Result<Json<dto::RecoveryStateResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let stored = entry
        .store
        .recovery_state(actor, parse(&run)?, parse(&session)?)
        .await?;
    Ok(Json(success(
        stored.value.map(episode).transpose()?,
        stored.cursor,
    )?))
}

async fn cancel(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::CancelRecoveryResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::CancelRecovery = read(&bytes)?;
    let stored = entry
        .store
        .cancel_recovery(
            actor,
            parse(&run)?,
            parse(&session)?,
            input.episode_id.as_str().to_owned(),
            reviewq_domain::identity::Revision::try_from(input.expected_revision.get())
                .map_err(|_| AppError::InvalidRequest)?,
        )
        .await?;
    Ok(Json(success(episode(stored.value)?, stored.cursor)?))
}

async fn interrupt(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<dto::InterruptResponse>, ApiError> {
    if !entry.sends_allowed {
        return Err(AppError::Unavailable.into());
    }
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::InterruptRequest = read(&bytes)?;
    let host = entry.host.as_ref().ok_or(AppError::Unavailable)?;
    let claim = entry
        .store
        .claim_interrupt(
            actor,
            parse(&run)?,
            parse(&session)?,
            parse(input.expected_turn_id.as_str())?,
        )
        .await?;
    let state = if claim.acquired {
        let observed = host
            .interrupt_managed(claim.target.clone(), claim.turn.clone())
            .await
            .unwrap_or(app::InterruptState::Deferred);
        entry.store.finish_interrupt(claim, observed).await?
    } else if matches!(
        claim.state,
        app::InterruptState::Uncertain | app::InterruptState::Reserved
    ) && host
        .observe_interrupt(claim.target.clone(), claim.turn.clone())
        .await
        .unwrap_or(false)
    {
        entry
            .store
            .finish_interrupt(claim, app::InterruptState::Interrupted)
            .await?
    } else {
        reviewq_app::events::Snapshot {
            value: claim.state,
            cursor: claim.cursor,
        }
    };
    let receipt = dto::InterruptReceipt {
        turn_id: input.expected_turn_id,
        state: match state.value {
            app::InterruptState::Interrupted => dto::InterruptState::Interrupted,
            app::InterruptState::Rejected => dto::InterruptState::Rejected,
            app::InterruptState::Deferred => dto::InterruptState::Deferred,
            _ => dto::InterruptState::Uncertain,
        },
    };
    Ok(Json(success(receipt, state.cursor)?))
}

fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, AppError> {
    if bytes.len() > 4096 {
        return Err(AppError::PayloadTooLarge);
    }
    serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)
}
fn project(value: app::Preferences) -> Result<dto::Preferences, AppError> {
    Ok(dto::Preferences {
        revision: RevisionDto::new(value.revision.get()).ok_or(AppError::Internal)?,
        auto_resume_codex_reviews: value.automatic,
    })
}
pub(in crate::entry) fn episode(value: app::Episode) -> Result<dto::Episode, AppError> {
    use app::RecoveryKind as Kind;
    use app::RecoveryState as Status;
    Ok(dto::Episode {
        episode_id: ResourceIdDto::new(value.id).ok_or(AppError::Internal)?,
        revision: RevisionDto::new(value.revision.get()).ok_or(AppError::Internal)?,
        failed_turn_id: HistoryTurnIdDto::new(value.turn.as_str()).ok_or(AppError::Internal)?,
        kind: match value.kind {
            Kind::UsageLimit => dto::Kind::UsageLimit,
            Kind::ModelCapacity => dto::Kind::ModelCapacity,
            Kind::Access => dto::Kind::Access,
        },
        state: match value.state {
            Status::Disabled => dto::State::Disabled,
            Status::Waiting => dto::State::Waiting,
            Status::Queued => dto::State::Queued,
            Status::Started => dto::State::Started,
            Status::Uncertain => dto::State::Uncertain,
            Status::Cancelled => dto::State::Cancelled,
            Status::Complete => dto::State::Complete,
        },
        reason: value.reason,
    })
}
