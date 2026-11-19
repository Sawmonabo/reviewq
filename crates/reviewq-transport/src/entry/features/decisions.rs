use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::decisions::{Decision, DecisionRequest, Disposition, execute_decision};
use reviewq_domain::identity::Revision;
use reviewq_protocol::decisions as dto;
use reviewq_protocol::{
    CheckpointIdDto, GitShaDto, ReviewIdDto, RevisionDto, Success, UtcTimestampDto,
};

use crate::entry::error::{ApiBytes, ApiPath as Path};
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

pub(super) async fn save(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::DecisionResult>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::DecisionRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let disposition = match request.disposition {
        dto::Disposition::Reviewed => Disposition::Reviewed,
        dto::Disposition::WaitingAuthor => Disposition::WaitingAuthor,
        dto::Disposition::NeedsReview => Disposition::NeedsReview,
    };
    respond(
        &entry,
        actor,
        DecisionRequest {
            review: parse(&review)?,
            submission: parse(request.submission_id.as_str())?,
            expected_revision: revision(request.expected_revision)?,
            expected_settings: revision(request.expected_settings_revision)?,
            source_sha: parse(request.source_sha.as_str())?,
            checkpoint_id: request.checkpoint_id.map(|id| id.as_str().to_owned()),
            disposition: Some(disposition),
        },
    )
    .await
}

pub(super) async fn checkpoint(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::DecisionResult>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::CheckpointRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    respond(
        &entry,
        actor,
        DecisionRequest {
            review: parse(&review)?,
            submission: parse(request.submission_id.as_str())?,
            expected_revision: revision(request.expected_revision)?,
            expected_settings: revision(request.expected_settings_revision)?,
            source_sha: parse(request.source_sha.as_str())?,
            checkpoint_id: Some(request.checkpoint_id.as_str().to_owned()),
            disposition: None,
        },
    )
    .await
}

async fn respond(
    entry: &Entry,
    actor: Actor,
    request: DecisionRequest,
) -> Result<Json<Success<dto::DecisionResult>>, ApiError> {
    let source = entry.active_source().await?.reader;
    let stored = execute_decision(
        entry.store.as_ref(),
        source.as_deref(),
        &entry.checkpoints,
        actor,
        request,
    )
    .await?;
    let result = stored.value;
    let data = dto::DecisionResult {
        review_id: ReviewIdDto::new(result.review.as_str()).ok_or(AppError::Internal)?,
        decision_revision: RevisionDto::new(result.decision.revision.get())
            .ok_or(AppError::Internal)?,
        decision: project(&result.decision)?,
        checkpoint_id: result
            .checkpoint_id
            .map(|value| CheckpointIdDto::new(value).ok_or(AppError::Internal))
            .transpose()?,
    };
    Ok(Json(success(data, stored.cursor)?))
}

pub(in crate::entry) fn project(value: &Decision) -> Result<dto::DecisionView, AppError> {
    Ok(dto::DecisionView {
        disposition: value.disposition.map(|value| match value {
            Disposition::Reviewed => dto::Disposition::Reviewed,
            Disposition::WaitingAuthor => dto::Disposition::WaitingAuthor,
            Disposition::NeedsReview => dto::Disposition::NeedsReview,
        }),
        reviewed_sha: value
            .reviewed_sha
            .as_ref()
            .map(|sha| GitShaDto::new(sha.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        reviewed_at: value
            .reviewed_at_ms
            .map(|time| UtcTimestampDto::from_unix_milliseconds(time).ok_or(AppError::Internal))
            .transpose()?,
    })
}

fn revision(value: RevisionDto) -> Result<Revision, AppError> {
    Revision::try_from(value.get()).map_err(|_| AppError::InvalidRequest)
}

pub(in crate::entry) fn project_attention(
    frontier: &reviewq_app::decisions::Frontier,
) -> Result<dto::Attention, AppError> {
    use reviewq_app::decisions::ObservedAttention;
    use reviewq_protocol::{ResourceIdDto, RunIdDto, SessionKeyDto};

    let resource = |value: &str| ResourceIdDto::new(value).ok_or(AppError::Internal);
    let run = |value: &str| RunIdDto::new(value).ok_or(AppError::Internal);
    let occurrence = |event: &ObservedAttention| -> Result<dto::ObservedAttention, AppError> {
        Ok(dto::ObservedAttention {
            event_id: resource(event.id.as_str())?,
            kind: event.kind.to_string(),
            label: event.label.clone(),
        })
    };
    let source = frontier
        .source
        .iter()
        .map(occurrence)
        .collect::<Result<_, _>>()?;
    let reports = frontier
        .reports
        .iter()
        .map(|report| {
            Ok::<_, AppError>(dto::ReportAttention {
                run_id: run(report.run.as_str())?,
                receipt_id: resource(report.receipt.as_str())?,
                attention_receipt_id: resource(report.attention.as_str())?,
            })
        })
        .collect::<Result<_, _>>()?;
    let sessions = frontier
        .sessions
        .iter()
        .map(|session| {
            Ok::<_, AppError>(dto::SessionAttention {
                run_id: run(session.run.as_str())?,
                session_key: SessionKeyDto::new(session.session.as_str())
                    .ok_or(AppError::Internal)?,
                event: occurrence(&session.event)?,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(dto::Attention {
        source,
        reports,
        sessions,
    })
}
