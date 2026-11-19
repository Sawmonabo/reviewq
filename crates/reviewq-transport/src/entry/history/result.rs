use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_protocol::history::{HistoryAvailability, HistoryResult, HistoryResultResponse};
use reviewq_protocol::{HistoryGenerationDto, RunIdDto, SessionKeyDto};

use crate::entry::pages::{parse, success};

use super::{ApiError, Entry, Path, encode_entry};

pub(super) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
) -> Result<Json<HistoryResultResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let source = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let result = entry
        .file_history
        .final_response(source.value.clone())
        .await?;
    if entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?
        .value
        != source.value
    {
        return Err(AppError::CursorExpired.into());
    }
    let (generation, availability, response) = match result {
        Some(result) => (
            Some(HistoryGenerationDto::new(result.generation).ok_or(AppError::Internal)?),
            HistoryAvailability::Available,
            result
                .entry
                .as_ref()
                .map(|response| encode_entry(&entry.auth.endpoint, &run, &session, response, 1))
                .transpose()?,
        ),
        None => (None, HistoryAvailability::Unavailable, None),
    };
    Ok(Json(success(
        HistoryResult {
            run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
            generation,
            availability,
            entry: response,
        },
        source.cursor,
    )?))
}
