use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::events::Snapshot;
use reviewq_app::history::ContentReference;
use reviewq_app::history::document::{Query as DocumentQuery, Window, WindowResult};
use reviewq_app::reports::{DISPLAY_INDEX_FORMAT, DisplayBlockKind};
use reviewq_domain::identity::{ReportHash, RunId, SessionKey};
use reviewq_protocol::history::{HistoryDocument, HistoryDocumentResult, HistoryText};
use reviewq_protocol::reports::ReportHashDto;
use reviewq_protocol::{HistoryEntryIdDto, HistoryGenerationDto, RunIdDto, SessionKeyDto};
use serde::Deserialize;

use crate::entry::diagrams::{self, Owner};
use crate::entry::document_display::{
    MAX_WINDOW_JSON_BYTES, project_block, project_fallback, project_table, remove_tables,
};
use crate::entry::error::{ApiError, ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{count, parse, success};

use super::{Entry, content::Locator};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(in crate::entry) struct WindowQuery {
    #[serde(rename = "ref")]
    reference: String,
    byte_offset: Option<u64>,
    sha256: Option<String>,
}

pub(in crate::entry) async fn document(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(query): Query<WindowQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let address = (parse(&run)?, parse(&session)?);
    let reference = Locator::decode(
        &entry.auth.endpoint,
        &query.reference,
        &address.0,
        &address.1,
    )?;
    let offset = query.byte_offset.unwrap_or(0);
    if offset != 0 && query.sha256.is_none() {
        return Err(AppError::CursorExpired.into());
    }
    let verified = read(
        &entry,
        actor.clone(),
        address,
        &reference,
        DocumentQuery {
            offset,
            expected: query.sha256.as_deref().map(parse).transpose()?,
        },
    )
    .await?;
    let Window {
        hash,
        bytes,
        byte_length,
        table_start,
        previous,
        next,
        result,
    } = verified.value;
    let font = diagrams::client_font(&headers);
    let drawing_owner = |byte_start| Owner::Native {
        run: run.clone(),
        session: session.clone(),
        source: query.reference.clone(),
        window: Some(diagrams::NativeWindowOwner {
            byte_start,
            sha256: hash.to_string(),
        }),
    };
    let result = match result {
        WindowResult::Ready(blocks) => {
            let mut output = Vec::with_capacity(blocks.len());
            let mut drawings_left = 8;
            let mut table_left = true;
            for block in blocks {
                let mut display = (drawings_left > 0)
                    .then(|| {
                        diagrams::observe(
                            &entry,
                            &actor,
                            drawing_owner(u64::from(block.span.byte_start)),
                            &block.markdown,
                            font.as_ref(),
                        )
                    })
                    .flatten();
                if let Some(display) = &mut display {
                    display.drawings.truncate(drawings_left);
                    drawings_left -= display.drawings.len();
                }
                if table_left
                    && block.span.kind == DisplayBlockKind::Table
                    && let Some(table) = entry.tables.read(&block.markdown).await
                {
                    crate::entry::tables::attach(&mut display, table);
                    table_left = false;
                }
                output.push(project_block(block, display)?);
            }
            HistoryDocumentResult::Ready { blocks: output }
        }
        WindowResult::Table(page) => HistoryDocumentResult::Table {
            page: project_table(page)?,
        },
        WindowResult::Source { reason, markdown } => {
            let display = (bytes.start == 0 && bytes.end == byte_length)
                .then(|| {
                    diagrams::observe(&entry, &actor, drawing_owner(0), &markdown, font.as_ref())
                })
                .flatten();
            HistoryDocumentResult::Source {
                reason: project_fallback(reason),
                markdown: HistoryText::new(markdown).ok_or(AppError::Internal)?,
                display,
            }
        }
    };
    let mut response = success(
        HistoryDocument {
            run_id: RunIdDto::new(run).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session).ok_or(AppError::Internal)?,
            entry_id: HistoryEntryIdDto::new(reference.item).ok_or(AppError::Internal)?,
            generation: HistoryGenerationDto::new(reference.generation)
                .ok_or(AppError::Internal)?,
            sha256: ReportHashDto::new(hash.as_str()).ok_or(AppError::Internal)?,
            index_format: count(u64::from(DISPLAY_INDEX_FORMAT))?,
            byte_length: count(byte_length)?,
            table_start: table_start.map(count).transpose()?,
            byte_start: count(bytes.start)?,
            byte_end: count(bytes.end)?,
            previous_byte: previous.map(count).transpose()?,
            next_byte: next.map(count).transpose()?,
            result,
        },
        verified.cursor,
    )?;
    let mut bytes = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    if bytes.len() > MAX_WINDOW_JSON_BYTES
        && let HistoryDocumentResult::Ready { blocks } = &mut response.data.result
    {
        remove_tables(blocks);
        bytes = serde_json::to_vec(&response).map_err(|_| AppError::Internal)?;
    }
    if bytes.len() > MAX_WINDOW_JSON_BYTES {
        return Err(AppError::Internal.into());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "private, no-store"),
        ],
        bytes,
    )
        .into_response())
}

pub(in crate::entry) async fn drawing_source(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    session: SessionKey,
    token: &str,
    offset: u64,
    expected: ReportHash,
) -> Result<String, AppError> {
    let reference = Locator::decode(&entry.auth.endpoint, token, &run, &session)?;
    let window = read(
        entry,
        actor,
        (run, session),
        &reference,
        DocumentQuery {
            offset,
            expected: Some(expected),
        },
    )
    .await?;
    match window.value.result {
        WindowResult::Ready(blocks) => blocks
            .into_iter()
            .find(|block| u64::from(block.span.byte_start) == offset)
            .map(|block| block.markdown)
            .ok_or(AppError::CursorExpired),
        WindowResult::Source { markdown, .. }
            if offset == 0 && window.value.bytes == (0..window.value.byte_length) =>
        {
            Ok(markdown)
        }
        _ => Err(AppError::CursorExpired),
    }
}

async fn read(
    entry: &Entry,
    actor: Actor,
    (run, session): (RunId, SessionKey),
    reference: &Locator,
    query: DocumentQuery,
) -> Result<Snapshot<Window>, AppError> {
    let selected = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let value = match reference.source()? {
        ContentReference::File(reference) => {
            entry
                .file_history
                .document(selected.value.clone(), reference, query)
                .await?
        }
        ContentReference::Rpc(reference) => {
            entry
                .host
                .as_ref()
                .ok_or(AppError::Unavailable)?
                .history_document(selected.value.clone(), reference, query)
                .await?
        }
    };
    if entry
        .store
        .history_session(actor, run, session)
        .await?
        .value
        != selected.value
    {
        return Err(AppError::CursorExpired);
    }
    Ok(Snapshot {
        value,
        cursor: selected.cursor,
    })
}
