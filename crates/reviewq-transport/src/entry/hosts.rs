use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::hosts::{NativeLink, link_codex};
use reviewq_protocol::queue::{ReviewSelection, ReviewSelectionResponse};
use reviewq_protocol::{EventCursorDto, Failure, ReviewIdDto, RunIdDto, SessionKeyDto, Success};
use serde::{Deserialize, Serialize};

use super::error::ApiBytes;
use super::{ApiError, Entry};

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
    entry.auth.endpoint.verify_bridge(&headers)?;
    if bytes.len() > 4_096
        || !headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.split(';').next() == Some("application/json"))
    {
        return Err(AppError::InvalidRequest.into());
    }
    let command: LinkCommand =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    if command.protocol_version != 1 {
        return Err(AppError::InvalidRequest.into());
    }
    let source = entry.source.as_deref().ok_or(AppError::Unavailable)?;
    let host = entry.host.as_deref().ok_or(AppError::Unavailable)?;
    let thread = command
        .thread_id
        .parse()
        .map_err(|_| AppError::InvalidRequest)?;
    let registered =
        match link_codex(entry.store.as_ref(), source, host, thread, command.target).await? {
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
    let target = registered.value;
    let response: ReviewSelectionResponse = Success {
        data: ReviewSelection::WithLead {
            review_id: ReviewIdDto::new(target.review_id().as_str()).ok_or(AppError::Internal)?,
            run_id: RunIdDto::new(target.run_id().as_str()).ok_or(AppError::Internal)?,
            lead_session_key: SessionKeyDto::new(target.session_key().as_str())
                .ok_or(AppError::Internal)?,
        },
        cursor: EventCursorDto::new(registered.cursor.get()).ok_or(AppError::Internal)?,
    };
    Ok(Json(response).into_response())
}
