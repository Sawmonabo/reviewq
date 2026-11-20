//! Resolve a private pasted passage against an exact authorized report version.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::hosts::MessageBody;
use reviewq_app::reports::{QuoteResolution, ReportReference};
use reviewq_protocol::{Success, reports as dto};

use super::resource;
use crate::entry::error::{ApiBytes, ApiPath as Path};
use crate::entry::pages::{count, parse, success};
use crate::entry::{ApiError, Entry};

pub(in crate::entry) async fn resolve(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, receipt)): Path<(String, String)>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::ResolvedQuote>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::QuoteResolveRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let reference = ReportReference {
        receipt_id: parse(&receipt)?,
        hash: parse(input.sha256.as_str())?,
    };
    let stored = entry
        .store
        .resolve_report_quote(
            actor,
            parse(&run)?,
            reference.clone(),
            MessageBody::new(input.text.as_str())?,
        )
        .await?;
    let result = match stored.value {
        QuoteResolution::Missing => dto::QuoteResolution::Missing,
        QuoteResolution::Ambiguous => dto::QuoteResolution::Ambiguous,
        QuoteResolution::Unique { offset, length } => dto::QuoteResolution::Unique {
            offset: count(offset)?,
            length: count(length)?,
            unit: dto::QuoteUnit::UnicodeCodepoints,
        },
    };
    Ok(Json(success(
        dto::ResolvedQuote {
            receipt_id: resource(reference.receipt_id.as_str())?,
            sha256: input.sha256,
            result,
        },
        stored.cursor,
    )?))
}
