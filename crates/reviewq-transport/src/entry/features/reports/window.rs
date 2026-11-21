//! Version-bound browser cursors and the accepted Findings window projection.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::reports::{
    DISPLAY_INDEX_FORMAT, DisplayBlockKind, DisplayWindowPosition, DisplayWindowState,
    ReportReference,
};
use reviewq_domain::identity::RunId;
use reviewq_protocol::reports as dto;
use serde::{Deserialize, Serialize};

use super::{cursor, summary};
use crate::entry::diagrams::{self, Owner};
use crate::entry::document_display::{
    MAX_WINDOW_JSON_BYTES, project_block, project_fallback, project_layout, remove_tables,
};
use crate::entry::error::{ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{count, parse, success};
use crate::entry::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(in crate::entry) struct WindowQuery {
    sha256: String,
    byte_offset: Option<u64>,
    byte_end: Option<u64>,
    table_byte_end: Option<u64>,
    cursor: Option<String>,
    #[serde(default)]
    layout: bool,
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
    let font = diagrams::client_font(&headers);
    let run: RunId = parse(&run)?;
    let report = ReportReference {
        receipt_id: parse(&receipt)?,
        hash: parse(&query.sha256)?,
    };
    let scope = format!(
        "report-window-blocks\0{run}\0{}\0{}\0{DISPLAY_INDEX_FORMAT}\0",
        report.receipt_id, report.hash,
    );
    if let Some(table_end) = query.table_byte_end
        && !query
            .byte_offset
            .zip(query.byte_end)
            .is_some_and(|(from, to)| from < table_end && table_end <= to)
    {
        return Err(AppError::InvalidRequest.into());
    }
    let endpoint = &entry.auth.endpoint;
    let position = match (
        query.byte_offset,
        query.byte_end,
        query.cursor,
        query.layout,
    ) {
        (None, None, Some(token), false) => {
            match endpoint.decode_cursor(scope.as_bytes(), &token)? {
                WindowCursor::Before(offset) => DisplayWindowPosition::Before(offset),
                WindowCursor::After(offset) => DisplayWindowPosition::After(offset),
            }
        }
        (offset, None, None, with_layout) => DisplayWindowPosition::AtByte {
            offset: offset.unwrap_or(0),
            with_layout,
        },
        (Some(from), Some(to), None, false) => DisplayWindowPosition::Exact { from, to },
        _ => return Err(AppError::InvalidRequest.into()),
    };
    let stored = entry
        .store
        .read_report_window(actor.clone(), run.clone(), report.clone(), position)
        .await?;
    let result = match stored.value.state {
        DisplayWindowState::Ready {
            total_blocks,
            blocks,
            previous,
            next,
            layout,
        } => {
            let token = |position| cursor(endpoint.encode_cursor(scope.as_bytes(), &position)?);
            let mut projected = Vec::with_capacity(blocks.len());
            let mut include_table = true;
            for block in blocks {
                let mut display = diagrams::observe(
                    &entry,
                    &actor,
                    Owner::report(&run, &report),
                    &block.markdown,
                    font.as_ref(),
                );
                if include_table
                    && block.span.kind == DisplayBlockKind::Table
                    && query
                        .table_byte_end
                        .is_none_or(|end| u64::from(block.span.byte_end) <= end)
                    && let Some(table) = entry.tables.read(&block.markdown).await
                {
                    include_table = false;
                    crate::entry::tables::attach(&mut display, table);
                }
                projected.push(project_block(block, display)?);
            }
            dto::DisplayResult::Ready {
                total_blocks: count(u64::from(total_blocks))?,
                blocks: projected,
                previous_cursor: previous.map(WindowCursor::Before).map(token).transpose()?,
                next_cursor: next.map(WindowCursor::After).map(token).transpose()?,
                layout: layout.map(project_layout).transpose()?,
            }
        }
        DisplayWindowState::Building => dto::DisplayResult::Building,
        DisplayWindowState::Fallback(reason) => dto::DisplayResult::Fallback {
            reason: project_fallback(reason),
        },
    };
    let mut response = success(
        dto::ReportWindow {
            report: summary(stored.value.report)?,
            index_format: count(u64::from(DISPLAY_INDEX_FORMAT))?,
            result,
        },
        stored.cursor,
    )?;
    let mut bytes = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    if bytes.len() > MAX_WINDOW_JSON_BYTES
        && let dto::DisplayResult::Ready { blocks, .. } = &mut response.data.result
    {
        remove_tables(blocks);
        bytes = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    }
    if bytes.len() > MAX_WINDOW_JSON_BYTES {
        return Err(AppError::Internal.into());
    }
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}
