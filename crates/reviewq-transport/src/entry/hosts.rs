use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::hosts::{ClaudeContext, NativeLink, link_codex, link_proven};
use reviewq_domain::session::{AbsolutePath, LeadTarget};
use reviewq_protocol::queue::{ReviewSelection, ReviewSelectionResponse};
use reviewq_protocol::{EventCursorDto, Failure, ReviewIdDto, RunIdDto, SessionKeyDto, Success};
use serde::{Deserialize, Serialize};

use super::error::ApiBytes;
use super::{ApiError, Entry};

pub(super) mod claude;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LinkCommand {
    protocol_version: u16,
    thread_id: String,
    target: String,
}

#[derive(Serialize)]
struct AmbiguousTarget {
    #[serde(flatten)]
    failure: Failure,
    candidates: Vec<String>,
}

pub(super) async fn link(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let command: LinkCommand = bridge_command(&entry, &headers, &bytes, 4_096)?;
    protocol(command.protocol_version)?;
    let source = entry.source_reader().await?;
    let host = entry.host.as_deref().ok_or(AppError::Unavailable)?;
    let thread = command
        .thread_id
        .parse()
        .map_err(|_| AppError::InvalidRequest)?;
    link_response(
        link_codex(
            entry.store.as_ref(),
            source.as_ref(),
            host,
            thread,
            command.target,
        )
        .await?,
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ClaudeCommand {
    protocol_version: u16,
    session_id: String,
    profile: std::path::PathBuf,
    cwd: std::path::PathBuf,
    transcript: Option<std::path::PathBuf>,
    target: Option<String>,
}

impl ClaudeCommand {
    fn context(&self) -> Result<ClaudeContext, AppError> {
        fn absolute(path: std::path::PathBuf) -> Result<AbsolutePath, AppError> {
            AbsolutePath::from_canonical(path).map_err(|_| AppError::InvalidRequest)
        }
        Ok(ClaudeContext {
            session_id: self
                .session_id
                .parse()
                .map_err(|_| AppError::InvalidRequest)?,
            profile: absolute(self.profile.clone())?,
            cwd: absolute(self.cwd.clone())?,
            transcript: self.transcript.clone().map(absolute).transpose()?,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeRegistration {
    protocol_version: u16,
    registration: Option<ReviewSelection>,
}

pub(super) async fn claude_registration(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let command: ClaudeCommand = bridge_command(&entry, &headers, &bytes, 16_384)?;
    protocol(command.protocol_version)?;
    if command.target.is_some() {
        return Err(AppError::InvalidRequest.into());
    }
    let companion = entry.claude.as_deref().ok_or(AppError::Unavailable)?;
    let registered = match companion.prove(command.context()?).await {
        Ok(proof) => Some(entry.store.lookup_native_lead(proof.binding).await?),
        Err(AppError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    let (registration, cursor) = match registered {
        Some(value) => (
            value.value.as_ref().map(selection).transpose()?,
            value.cursor,
        ),
        None => (None, *entry.commits.borrow()),
    };
    Ok(Json(Success {
        data: ClaudeRegistration {
            protocol_version: 1,
            registration,
        },
        cursor: EventCursorDto::new(cursor.get()).ok_or(AppError::Internal)?,
    })
    .into_response())
}

pub(super) async fn claude_link(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Response, ApiError> {
    let command: ClaudeCommand = bridge_command(&entry, &headers, &bytes, 16_384)?;
    protocol(command.protocol_version)?;
    let companion = entry.claude.as_deref().ok_or(AppError::Unavailable)?;
    let source = entry.source_reader().await?;
    let proof = companion.prove(command.context()?).await?;
    let target = command.target.ok_or(AppError::InvalidRequest)?;
    link_response(link_proven(entry.store.as_ref(), source.as_ref(), proof, target).await?)
}

fn bridge_command<T: serde::de::DeserializeOwned>(
    entry: &Entry,
    headers: &HeaderMap,
    body: &[u8],
    limit: usize,
) -> Result<T, ApiError> {
    entry.auth.endpoint.verify_bridge(headers)?;
    if body.len() > limit
        || !headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(';').next() == Some("application/json"))
    {
        return Err(AppError::InvalidRequest.into());
    }
    serde_json::from_slice(body).map_err(|_| AppError::InvalidRequest.into())
}

fn protocol(version: u16) -> Result<(), ApiError> {
    if version != 1 {
        return Err(AppError::PreconditionFailed.into());
    }
    Ok(())
}

fn link_response(result: NativeLink) -> Result<Response, ApiError> {
    let registered = match result {
        NativeLink::Linked(registered) => registered,
        NativeLink::Ambiguous(candidates) => {
            let (_, failure) = super::error::failure(AppError::Conflict);
            return Ok((
                StatusCode::CONFLICT,
                Json(AmbiguousTarget {
                    failure,
                    candidates,
                }),
            )
                .into_response());
        }
    };
    let response: ReviewSelectionResponse = Success {
        data: selection(&registered.value)?,
        cursor: EventCursorDto::new(registered.cursor.get()).ok_or(AppError::Internal)?,
    };
    Ok(Json(response).into_response())
}

fn selection(target: &LeadTarget) -> Result<ReviewSelection, AppError> {
    Ok(ReviewSelection::WithLead {
        review_id: ReviewIdDto::new(target.review_id().as_str()).ok_or(AppError::Internal)?,
        run_id: RunIdDto::new(target.run_id().as_str()).ok_or(AppError::Internal)?,
        lead_session_key: SessionKeyDto::new(target.session_key().as_str())
            .ok_or(AppError::Internal)?,
    })
}
