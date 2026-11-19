use std::sync::Arc;

use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::decisions::{DecisionRequest, TerminalDecision};
use reviewq_app::notifications::NotificationAction;
use reviewq_domain::identity::{GitSha, SubmissionId};
use reviewq_protocol::decisions as decision_dto;
use reviewq_protocol::{ResourceIdDto, Success};
use serde::{Deserialize, Serialize};

use super::control::ControlRequest;
use super::error::ApiBytes;
use super::features::{decisions, notifications};
use super::pages::parse;
use super::targets::{self, ResolvedReview};
use super::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ack {
    ids: Vec<ResourceIdDto>,
}

pub(super) async fn review(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let proof = authenticate(&entry, &uri, &headers, &bytes)?;
    let result = decide(&entry, &bytes).await;
    signed(&entry, &proof, result)
}

async fn decide(
    entry: &Entry,
    bytes: &[u8],
) -> Result<Success<decision_dto::DecisionResult>, AppError> {
    let body: decision_dto::TerminalDecision =
        serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)?;
    if body.target.is_empty()
        || body.target.len() > 2048
        || body.target.chars().any(char::is_control)
    {
        return Err(AppError::InvalidRequest);
    }
    let actor = Actor::local_operator();
    let submission: SubmissionId = parse(body.submission_id.as_str())?;
    let source_sha: GitSha = parse(body.source_sha.as_str())?;
    let disposition = decisions::domain_disposition(body.disposition);
    if let Some(previous) = entry
        .store
        .terminal_decision_replay(
            actor.clone(),
            TerminalDecision {
                target: body.target.clone(),
                submission: submission.clone(),
                source_sha: source_sha.clone(),
                disposition,
            },
        )
        .await?
    {
        return decisions::project_result(previous);
    }
    let review = match targets::resolve(entry, &actor, &body.target).await? {
        ResolvedReview::Review(id) => id,
        ResolvedReview::Alternatives(_) => return Err(AppError::Conflict),
    };
    let snapshot = entry
        .store
        .read_review(actor.clone(), review.clone(), None, 1)
        .await?;
    let stored = decisions::execute(
        entry,
        actor,
        DecisionRequest {
            review,
            submission,
            source_sha,
            disposition: Some(disposition),
            checkpoint_id: None,
            terminal_target: Some(body.target),
            expected_revision: snapshot.value.item.decision.revision,
            expected_settings: snapshot.value.settings_revision,
        },
    )
    .await?;
    decisions::project_result(stored)
}

pub(super) async fn ack(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let proof = authenticate(&entry, &uri, &headers, &bytes)?;
    let result = async {
        let body: Ack = serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
        let ids = body
            .ids
            .iter()
            .map(|id| parse(id.as_str()))
            .collect::<Result<Vec<_>, _>>()?;
        let changed = entry
            .store
            .assign_notifications(Actor::local_operator(), NotificationAction::Read, ids)
            .await?;
        notifications::project_change(changed)
    }
    .await;
    signed(&entry, &proof, result)
}

fn authenticate<'a>(
    entry: &Entry,
    uri: &'a Uri,
    headers: &HeaderMap,
    bytes: &[u8],
) -> Result<ControlRequest<'a>, AppError> {
    let target = uri
        .path_and_query()
        .ok_or(AppError::InvalidRequest)?
        .as_str();
    entry.auth.control("POST", target, headers, bytes)
}

fn signed(
    entry: &Entry,
    proof: &ControlRequest<'_>,
    result: Result<impl Serialize, AppError>,
) -> Result<Response, ApiError> {
    let (status, body) = match result {
        Ok(value) => (StatusCode::OK, serde_json::to_vec(&value)),
        Err(error) => {
            let (status, failure) = super::error::failure(error);
            (status, serde_json::to_vec(&failure))
        }
    };
    let body = body.map_err(|_| AppError::Internal)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    entry
        .auth
        .endpoint
        .response_proof(proof, status.as_u16(), &mut headers, &body)?;
    Ok((status, headers, body).into_response())
}
