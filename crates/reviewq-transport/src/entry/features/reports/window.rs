//! Version-bound browser cursors and the accepted Findings window projection.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::reports::{
    DISPLAY_INDEX_FORMAT, DisplayBlockKind, DisplayFallback, DisplayWindowBlock,
    DisplayWindowPosition, DisplayWindowState, ReportReference,
};
use reviewq_domain::identity::RunId;
use reviewq_protocol::reports as dto;
use serde::{Deserialize, Serialize};

use super::{cursor, summary};
use crate::entry::error::{ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{count, parse, success};
use crate::entry::{ApiError, Entry};

const MAX_WINDOW_JSON_BYTES: usize = 7 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(in crate::entry) struct WindowQuery {
    sha256: String,
    byte_offset: Option<u64>,
    cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum WindowCursor {
    Before(u32),
    After(u32),
}

pub(in crate::entry) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, receipt)): Path<(String, String)>,
    Query(query): Query<WindowQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let report = ReportReference {
        receipt_id: parse(&receipt)?,
        hash: parse(&query.sha256)?,
    };
    let scope = format!(
        "report-window-blocks\0{run}\0{}\0{}\0{DISPLAY_INDEX_FORMAT}\0",
        report.receipt_id, report.hash,
    );
    let endpoint = &entry.auth.endpoint;
    let position = match (query.byte_offset, query.cursor) {
        (None, Some(token)) => match endpoint.decode_cursor(scope.as_bytes(), &token)? {
            WindowCursor::Before(offset) => DisplayWindowPosition::Before(offset),
            WindowCursor::After(offset) => DisplayWindowPosition::After(offset),
        },
        (offset, None) => DisplayWindowPosition::AtByte(offset.unwrap_or(0)),
        _ => return Err(AppError::InvalidRequest.into()),
    };
    let stored = entry
        .store
        .read_report_window(actor, run, report, position)
        .await?;
    let result = match stored.value.state {
        DisplayWindowState::Ready {
            total_blocks,
            blocks,
            previous,
            next,
        } => {
            let token = |position| cursor(endpoint.encode_cursor(scope.as_bytes(), &position)?);
            dto::DisplayResult::Ready {
                total_blocks: count(u64::from(total_blocks))?,
                blocks: blocks
                    .into_iter()
                    .map(project_block)
                    .collect::<Result<_, _>>()?,
                previous_cursor: previous.map(WindowCursor::Before).map(token).transpose()?,
                next_cursor: next.map(WindowCursor::After).map(token).transpose()?,
            }
        }
        DisplayWindowState::Building => dto::DisplayResult::Building,
        DisplayWindowState::Fallback(reason) => dto::DisplayResult::Fallback {
            reason: project_fallback(reason),
        },
    };
    let response = success(
        dto::ReportWindow {
            report: summary(stored.value.report)?,
            index_format: count(u64::from(DISPLAY_INDEX_FORMAT))?,
            result,
        },
        stored.cursor,
    )?;
    let bytes = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    if bytes.len() > MAX_WINDOW_JSON_BYTES {
        return Err(AppError::Internal.into());
    }
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

fn project_block(block: DisplayWindowBlock) -> Result<dto::DisplayBlock, AppError> {
    use dto::DisplayBlockKind as Kind;
    let (kind, heading_level) = match block.span.kind {
        DisplayBlockKind::Paragraph => (Kind::Paragraph, None),
        DisplayBlockKind::Heading(level) => (Kind::Heading, Some(level)),
        DisplayBlockKind::BlockQuote => (Kind::BlockQuote, None),
        DisplayBlockKind::Code => (Kind::Code, None),
        DisplayBlockKind::List => (Kind::List, None),
        DisplayBlockKind::Rule => (Kind::Rule, None),
        DisplayBlockKind::Table => (Kind::Table, None),
    };
    Ok(dto::DisplayBlock {
        byte_start: count(u64::from(block.span.byte_start))?,
        byte_end: count(u64::from(block.span.byte_end))?,
        scalar_start: count(u64::from(block.span.scalar_start))?,
        kind,
        heading_level: heading_level
            .map(|level| count(u64::from(level)))
            .transpose()?,
        markdown: block.markdown,
    })
}

fn project_fallback(reason: DisplayFallback) -> dto::DisplayFallback {
    use dto::DisplayFallback as Reason;
    match reason {
        DisplayFallback::DocumentBudget => Reason::DocumentBudget,
        DisplayFallback::ParserBudget => Reason::ParserBudget,
        DisplayFallback::DocumentContext => Reason::DocumentContext,
        DisplayFallback::UnsupportedSource => Reason::UnsupportedSource,
        DisplayFallback::InvalidBoundary => Reason::InvalidBoundary,
        DisplayFallback::ParserFailed => Reason::ParserFailed,
        DisplayFallback::IncompleteSource => Reason::IncompleteSource,
        DisplayFallback::CacheUnavailable => Reason::CacheUnavailable,
    }
}
