use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use reviewq_app::access::AppError;
use reviewq_app::events::Snapshot;
use reviewq_app::hosts::{DeliveryOutcome, DeliveryReceipt, MessageBody, receipt, send_message};
use reviewq_protocol::{
    DeliveryReceipt as ReceiptDto, DeliveryReceiptResponse, DeliveryState, EventCursorDto,
    HistoryEntryIdDto, HistoryTurnIdDto, ObservedInputDto, RunIdDto, SendMessageRequest,
    SessionKeyDto, SubmissionIdDto, Success, UtcTimestampDto,
};

use super::error::{ApiBytes, ApiPath as Path};
use super::pages::parse;
use super::{ApiError, Entry};

pub(super) async fn send(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    ApiBytes(bytes): ApiBytes,
) -> Result<(StatusCode, Json<DeliveryReceiptResponse>), ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    if !entry.sends_allowed {
        return Err(AppError::Forbidden.into());
    }
    let input: SendMessageRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let result = send_message(
        entry.store.as_ref(),
        actor,
        parse(&run)?,
        parse(&session)?,
        parse(input.submission_id.as_str())?,
        MessageBody::new(input.text.as_str())?,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(project(result)?)))
}

pub(super) async fn get(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session, submission)): Path<(String, String, String)>,
) -> Result<Json<DeliveryReceiptResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let result = receipt(
        entry.store.as_ref(),
        actor,
        parse(&run)?,
        parse(&session)?,
        parse(&submission)?,
    )
    .await?;
    Ok(Json(project(result)?))
}

pub(super) fn project(
    stored: Snapshot<DeliveryReceipt>,
) -> Result<DeliveryReceiptResponse, AppError> {
    let row = stored.value;
    let state = match row.outcome {
        DeliveryOutcome::Queued => DeliveryState::Queued { observed_input: () },
        DeliveryOutcome::Dispatching => DeliveryState::Dispatching { observed_input: () },
        DeliveryOutcome::Started(input) => DeliveryState::Started {
            observed_input: ObservedInputDto {
                turn_id: HistoryTurnIdDto::new(input.turn_id.as_str()).ok_or(AppError::Internal)?,
                entry_id: HistoryEntryIdDto::new(input.entry_id.as_str())
                    .ok_or(AppError::Internal)?,
            },
        },
        DeliveryOutcome::Rejected => DeliveryState::Rejected { observed_input: () },
        DeliveryOutcome::Uncertain => DeliveryState::Uncertain { observed_input: () },
    };
    Ok(Success {
        cursor: EventCursorDto::new(stored.cursor.get()).ok_or(AppError::Internal)?,
        data: ReceiptDto {
            submission_id: SubmissionIdDto::new(row.submission_id.as_str())
                .ok_or(AppError::Internal)?,
            run_id: RunIdDto::new(row.target.run_id().as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(row.target.session_key().as_str())
                .ok_or(AppError::Internal)?,
            state,
            created_at: UtcTimestampDto::from_unix_milliseconds(row.created_at_ms)
                .ok_or(AppError::Internal)?,
            updated_at: UtcTimestampDto::from_unix_milliseconds(row.updated_at_ms)
                .ok_or(AppError::Internal)?,
        },
    })
}
