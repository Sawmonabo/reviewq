use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use reviewq_app::access::AppError;
use reviewq_app::hosts::{MessageBody, ReportQuote, ReportRef};
use reviewq_app::reports::{DispositionEdit, Publication, ViewedReport};
use reviewq_protocol::reports as dto;
use reviewq_protocol::{DeliveryReceiptResponse, RevisionDto, Success};

use crate::entry::error::{ApiBytes, ApiPath as Path};
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

use super::{descriptor, resource};

pub(in crate::entry::features) async fn set_disposition(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::DispositionReceipt>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::DispositionRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let stored = entry
        .store
        .set_disposition(
            actor,
            DispositionEdit {
                run: parse(&run)?,
                receipt: parse(input.receipt_id.as_str())?,
                expected_revision: input
                    .expected_version
                    .get()
                    .try_into()
                    .map_err(|_| AppError::InvalidRequest)?,
                finding_id: input.finding_id.as_str().to_owned(),
                submission: parse(input.submission_id.as_str())?,
                blocking: input.disposition.blocking,
                publication: input.disposition.publication.map(|choice| match choice {
                    dto::PublicationChoice::Withheld => Publication::Withheld,
                    dto::PublicationChoice::Authorized => Publication::Authorized,
                    dto::PublicationChoice::Published => Publication::Published,
                }),
                reason: input.disposition.reason,
            },
        )
        .await?;
    Ok(Json(success(
        dto::DispositionReceipt {
            receipt_id: resource(stored.value.receipt_id.as_str())?,
            user_disposition_version: RevisionDto::new(stored.value.disposition_version.get())
                .ok_or(AppError::Internal)?,
            finding: descriptor(stored.value.finding)?,
        },
        stored.cursor,
    )?))
}

pub(in crate::entry::features) async fn viewed(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::ViewedReceipt>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::ViewedRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let stored = entry
        .store
        .mark_report_viewed(
            actor,
            ViewedReport {
                run: parse(&run)?,
                receipt: parse(input.receipt_id.as_str())?,
                submission: parse(input.submission_id.as_str())?,
                rendered_batch_id: input.rendered_batch_id.map(|id| id.as_str().to_owned()),
            },
        )
        .await?;
    Ok(Json(success(
        dto::ViewedReceipt {
            receipt_id: resource(stored.value.receipt_id.as_str())?,
            previous_receipt_id: stored
                .value
                .previous_receipt_id
                .map(|id| resource(id.as_str()))
                .transpose()?,
            rendered_batch_id: stored
                .value
                .rendered_batch_id
                .map(|id| resource(&id))
                .transpose()?,
        },
        stored.cursor,
    )?))
}

pub(in crate::entry::features) async fn feedback(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<(StatusCode, Json<DeliveryReceiptResponse>), ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    if !entry.sends_allowed {
        return Err(AppError::Forbidden.into());
    }
    let run = parse(&run)?;
    let input: dto::FeedbackRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let reference = feedback_reference(
        input.receipt_id,
        input.report_hash,
        &input.quote,
        input.quote_ref,
    )?;
    let receipt = reviewq_app::reports::send_feedback(
        entry.store.as_ref(),
        actor,
        run,
        parse(input.submission_id.as_str())?,
        MessageBody::new(input.text.as_str())?,
        reference,
    )
    .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::entry::messages::project(receipt)?),
    ))
}

pub(in crate::entry) fn feedback_reference(
    receipt_id: reviewq_protocol::ResourceIdDto,
    report_hash: dto::ReportHashDto,
    text: &str,
    range: Option<dto::QuoteRef>,
) -> Result<ReportRef, AppError> {
    let quote = match range {
        Some(range) => {
            let quote = ReportQuote::new(MessageBody::new(text)?, range.offset.get())?;
            if quote.length() != range.length.get() {
                return Err(AppError::InvalidRequest);
            }
            Some(quote)
        }
        None if text.is_empty() => None,
        None => return Err(AppError::InvalidRequest),
    };
    Ok(ReportRef {
        receipt_id: parse(receipt_id.as_str())?,
        report_hash: parse(report_hash.as_str())?,
        quote,
    })
}
