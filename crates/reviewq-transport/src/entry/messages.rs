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

pub(in crate::entry) fn project(
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
    let report_ref = row
        .report_ref
        .map(|reference| {
            let resource = |id: &reviewq_domain::identity::ReceiptId| {
                reviewq_protocol::ResourceIdDto::new(id.as_str()).ok_or(AppError::Internal)
            };
            Ok::<_, AppError>(reviewq_protocol::DeliveryReportRef {
                receipt_id: resource(&reference.receipt_id)?,
                report_hash: reviewq_protocol::reports::ReportHashDto::new(
                    reference.report_hash.as_str(),
                )
                .ok_or(AppError::Internal)?,
                current_receipt_id: reference
                    .current_receipt_id
                    .as_ref()
                    .map(resource)
                    .transpose()?,
            })
        })
        .transpose()?;
    Ok(Success {
        cursor: EventCursorDto::new(stored.cursor.get()).ok_or(AppError::Internal)?,
        data: ReceiptDto {
            submission_id: SubmissionIdDto::new(row.submission_id.as_str())
                .ok_or(AppError::Internal)?,
            run_id: RunIdDto::new(row.target.run_id().as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(row.target.session_key().as_str())
                .ok_or(AppError::Internal)?,
            state,
            report_ref,
            created_at: UtcTimestampDto::from_unix_milliseconds(row.created_at_ms)
                .ok_or(AppError::Internal)?,
            updated_at: UtcTimestampDto::from_unix_milliseconds(row.updated_at_ms)
                .ok_or(AppError::Internal)?,
        },
    })
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MessageQuery {
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct MessageCursor(i64, String);

pub(super) async fn list(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    super::error::ApiQuery(query): super::error::ApiQuery<MessageQuery>,
) -> Result<Json<Success<reviewq_protocol::MessageHistoryPage>>, ApiError> {
    use reviewq_app::hosts::{MAX_MESSAGE_HISTORY_PAGE, MessagePosition};
    use reviewq_domain::identity::{RunId, SessionKey};
    use reviewq_protocol::{Page, PageCursorDto};

    let actor = entry.auth.browser_actor(&headers, false)?;
    let limit = query.limit.unwrap_or(MAX_MESSAGE_HISTORY_PAGE);
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let scope = serde_json::to_vec(&("browser-messages", run.as_str(), session.as_str(), limit))
        .map_err(|_| AppError::Internal)?;
    let endpoint = &entry.auth.endpoint;
    let after = query
        .cursor
        .as_deref()
        .map(|token| {
            let MessageCursor(time, id) = endpoint.decode_cursor(&scope, token)?;
            Ok::<_, AppError>(MessagePosition {
                created_at_ms: time,
                message_id: parse(&id)?,
            })
        })
        .transpose()?;
    let stored = entry
        .store
        .read_messages(actor, run, session, after, limit)
        .await?;
    let mut budget: usize = 512 * 1024 - 4096;
    let mut last = None;
    let mut has_more = stored.value.has_more;
    let mut items = Vec::new();
    for message in stored.value.items {
        let position = MessageCursor(
            message.position.created_at_ms,
            message.position.message_id.to_string(),
        );
        let item = project_message(message)?;
        let charge = serde_json::to_vec(&item)
            .map_err(|_| AppError::Internal)?
            .len()
            + 1;
        let Some(remaining) = budget.checked_sub(charge) else {
            has_more = true;
            break;
        };
        budget = remaining;
        items.push(item);
        last = Some(position);
    }
    let next_cursor = has_more
        .then(|| {
            let last = last.ok_or(AppError::PayloadTooLarge)?;
            PageCursorDto::new(endpoint.encode_cursor(&scope, &last)?).ok_or(AppError::Internal)
        })
        .transpose()?;
    let page = Page::new(items, next_cursor).ok_or(AppError::Internal)?;
    Ok(Json(super::pages::success(page, stored.cursor)?))
}

fn project_message(
    value: reviewq_app::hosts::RecordedMessage,
) -> Result<reviewq_protocol::MessageHistoryItem, AppError> {
    use super::pages::count;
    use reviewq_app::hosts::{DeliveryState as State, RecordedPurpose as Purpose};
    use reviewq_protocol::{MessageAttachment, MessageHistoryItem, RecordedPurpose, RecordedState};
    use reviewq_protocol::{ResourceIdDto, reports::ReportHashDto};

    let resource = |value: reviewq_domain::identity::ReceiptId| {
        ResourceIdDto::new(value.as_str()).ok_or(AppError::Internal)
    };
    let attachment = value
        .attachment
        .map(|value| -> Result<_, AppError> {
            Ok(MessageAttachment {
                receipt_id: value.receipt_id.map(resource).transpose()?,
                report_hash: value
                    .report_hash
                    .map(|hash| ReportHashDto::new(hash.as_str()).ok_or(AppError::Internal))
                    .transpose()?,
                current_receipt_id: value.current_receipt_id.map(resource).transpose()?,
                quote: value.quote,
                quote_offset: value.quote_offset.map(count).transpose()?,
            })
        })
        .transpose()?;
    Ok(MessageHistoryItem {
        message_id: ResourceIdDto::new(value.position.message_id.as_str())
            .ok_or(AppError::Internal)?,
        submission_id: SubmissionIdDto::new(value.submission_id.as_str())
            .ok_or(AppError::Internal)?,
        purpose: match value.purpose {
            Purpose::User => RecordedPurpose::User,
            Purpose::ReportFeedback => RecordedPurpose::ReportFeedback,
            Purpose::CodexRecovery => RecordedPurpose::CodexRecovery,
            Purpose::ThreadFollowup => RecordedPurpose::ThreadFollowup,
            Purpose::Publication => RecordedPurpose::Publication,
        },
        state: match value.state {
            State::Queued => RecordedState::Queued,
            State::Dispatching => RecordedState::Dispatching,
            State::Started => RecordedState::Started,
            State::Rejected => RecordedState::Rejected,
            State::Uncertain => RecordedState::Uncertain,
        },
        outcome_detail: value.outcome_detail,
        followup_batch_id: value
            .followup_batch_id
            .map(|id| ResourceIdDto::new(id).ok_or(AppError::Internal))
            .transpose()?,
        text: value.text,
        attachment,
        native_turn_id: value
            .native_turn_id
            .map(|id| HistoryTurnIdDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        native_entry_id: value
            .native_entry_id
            .map(|id| HistoryEntryIdDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        service_admitted: value.service_admitted,
        created_at: UtcTimestampDto::from_unix_milliseconds(value.position.created_at_ms)
            .ok_or(AppError::Internal)?,
        updated_at: UtcTimestampDto::from_unix_milliseconds(value.updated_at_ms)
            .ok_or(AppError::Internal)?,
    })
}
