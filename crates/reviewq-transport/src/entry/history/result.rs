use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_protocol::history::{
    ChildResultFailure, HistoryAvailability, HistoryResult, HistoryResultResponse,
};
use reviewq_protocol::{
    HistoryGenerationDto, HistoryTurnIdDto, RunIdDto, SessionKeyDto, UtcTimestampDto,
};

use crate::entry::pages::{parse, success};

use super::drawing_hints::InlineDisplay;
use super::{ApiError, Entry, Path, Projection, encode_entry};

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
    let failure = match result.as_ref().and_then(|native| native.failure.as_ref()) {
        Some(failure) if source.value.parent.is_some() => {
            let report = entry
                .store
                .read_report(actor.clone(), run.clone(), None, false)
                .await?;
            Some(ChildResultFailure {
                turn_id: HistoryTurnIdDto::new(failure.turn.as_str()).ok_or(AppError::Internal)?,
                completed_at: UtcTimestampDto::from_unix_milliseconds(failure.completed_at_ms)
                    .ok_or(AppError::Internal)?,
                followed_by_lead_report: report
                    .value
                    .report
                    .is_some_and(|report| report.captured_at_ms > failure.completed_at_ms),
            })
        }
        _ => None,
    };
    if entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?
        .value
        != source.value
    {
        return Err(AppError::CursorExpired.into());
    }
    let projection = Projection {
        endpoint: &entry.auth.endpoint,
        run: &run,
        session: &session,
        display: Some(InlineDisplay {
            entry: &entry,
            actor: &actor,
            run: &run,
            session: &session,
            font: crate::entry::diagrams::client_font(&headers),
        }),
    };
    let (generation, availability, response) = match result {
        Some(result) => (
            Some(HistoryGenerationDto::new(result.generation.as_str()).ok_or(AppError::Internal)?),
            HistoryAvailability::Available,
            result
                .entry
                .as_ref()
                .map(|response| encode_entry(&projection, response, &result.generation, 1))
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
            failure,
        },
        source.cursor,
    )?))
}
