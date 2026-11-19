use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::{
    HistorySession, NativeHistoryPage, NativePosition, TurnWindow, VisibleEntry, VisibleKind,
};
use reviewq_domain::identity::{EventCursor, RunId, SessionKey};
use reviewq_protocol::history::{
    HistoryAvailability, HistoryEntry, HistoryPage, HistoryPageEncodeError, HistoryPageResponse,
    HistoryPart, HistoryText, HistoryWindow,
};
use reviewq_protocol::{
    EventCursorDto, HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, HistoryTurnIdDto,
    RunIdDto, SessionKeyDto, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::error::{ApiPath as Path, ApiQuery as Query};
use super::{ApiError, Entry, ServiceEndpoint};

const CURSOR: &[u8] = b"reviewq-native-history-v1";
const PREVIEW_BYTES: usize = 40 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HistoryQuery {
    pub(super) limit: Option<u16>,
    pub(super) cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Cursor {
    run: String,
    session: String,
    generation: String,
    limit: u16,
    native: Option<String>,
    ascending: bool,
    skip_turn: Option<String>,
    within: Option<Within>,
    file_offset: Option<u64>,
}

#[derive(Deserialize, Serialize)]
struct Within {
    turn: String,
    boundary: usize,
    forward: bool,
}

pub(super) async fn page(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let body = read(
        &entry,
        actor,
        super::pages::parse(&run)?,
        super::pages::parse(&session)?,
        query,
    )
    .await?;
    Ok(([(header::CONTENT_TYPE, "application/json")], body).into_response())
}

pub(super) async fn read(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    session: SessionKey,
    query: HistoryQuery,
) -> Result<Vec<u8>, AppError> {
    let limit = query.limit.unwrap_or(100);
    if !(1..=100).contains(&limit) {
        return Err(AppError::InvalidRequest);
    }
    let endpoint = &entry.auth.endpoint;
    let cursor = query
        .cursor
        .as_deref()
        .map(|token| endpoint.decode_cursor::<Cursor>(CURSOR, token))
        .transpose()?;
    if cursor.as_ref().is_some_and(|cursor| {
        cursor.run != run.as_str() || cursor.session != session.as_str() || cursor.limit != limit
    }) {
        return Err(AppError::CursorExpired);
    }
    let lead = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let position = cursor
        .as_ref()
        .map(Cursor::native_position)
        .transpose()?
        .unwrap_or_default();
    let mut reset = false;
    let history = native_page(entry, lead.value.clone(), position, limit).await;
    let history = match history {
        Ok(page)
            if cursor
                .as_ref()
                .is_none_or(|cursor| cursor.generation == page.generation) =>
        {
            page
        }
        Ok(_) | Err(AppError::CursorExpired) if cursor.is_some() => {
            reset = true;
            native_page(entry, lead.value, NativePosition::default(), limit).await?
        }
        Err(error) => return Err(error),
        Ok(page) => page,
    };
    entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?;
    encode(endpoint, &run, &session, lead.cursor, limit, history, reset)
}

async fn native_page(
    entry: &Entry,
    session: HistorySession,
    position: NativePosition,
    limit: u16,
) -> Result<NativeHistoryPage, AppError> {
    if let Some(page) = entry
        .file_history
        .page(session.clone(), position.clone(), limit)
        .await?
    {
        return Ok(page);
    }
    let host = entry.host.as_ref().ok_or(AppError::Unavailable)?;
    host.history(session, position, limit).await
}

fn encode(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    event: EventCursor,
    limit: u16,
    history: NativeHistoryPage,
    reset: bool,
) -> Result<Vec<u8>, AppError> {
    let cursor = |position: Option<NativePosition>| {
        position
            .map(|position| {
                let witness = Cursor::new(run, session, &history.generation, limit, position);
                HistoryCursorDto::new(endpoint.encode_cursor(CURSOR, &witness)?)
                    .ok_or(AppError::Internal)
            })
            .transpose()
    };
    let mut trim = 0;
    loop {
        let data = HistoryPage {
            run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
            generation: HistoryGenerationDto::new(history.generation.as_str())
                .ok_or(AppError::Internal)?,
            availability: HistoryAvailability::Available,
            reset,
            retry_after_ms: None,
            window: HistoryWindow::new(
                history
                    .entries
                    .iter()
                    .skip(trim)
                    .map(encode_entry)
                    .collect::<Result<Vec<_>, _>>()?,
                cursor(history.older_after_trim(trim))?,
                cursor(history.newer.clone())?,
            )
            .ok_or(AppError::Internal)?,
        };
        let result = HistoryPageResponse::new(
            data,
            EventCursorDto::new(event.get()).ok_or(AppError::Internal)?,
        )
        .encode();
        match result {
            Ok(bytes) => return Ok(bytes),
            Err(HistoryPageEncodeError::TooLarge) if history.entries.len() - trim > 1 => {
                trim += (history.entries.len() - trim) / 2;
            }
            Err(HistoryPageEncodeError::TooLarge) => return Err(AppError::PayloadTooLarge),
            Err(HistoryPageEncodeError::Serialization(_)) => return Err(AppError::Internal),
        }
    }
}

fn encode_entry(entry: &VisibleEntry) -> Result<HistoryEntry, AppError> {
    let end = entry
        .text
        .floor_char_boundary(PREVIEW_BYTES.min(entry.text.len()));
    let text = HistoryText::new(&entry.text[..end]).ok_or(AppError::Internal)?;
    let part = match entry.kind {
        VisibleKind::User => HistoryPart::UserPrompt { text },
        VisibleKind::Assistant => HistoryPart::AssistantText { text },
    };
    Ok(HistoryEntry {
        entry_id: HistoryEntryIdDto::new(entry.id.as_str()).ok_or(AppError::Internal)?,
        turn_id: entry
            .turn
            .as_ref()
            .map(|turn| HistoryTurnIdDto::new(turn.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        timestamp: entry
            .at_ms
            .and_then(UtcTimestampDto::from_unix_milliseconds),
        parts: vec![part],
    })
}

impl Cursor {
    fn new(
        run: &RunId,
        session: &SessionKey,
        generation: &str,
        limit: u16,
        position: NativePosition,
    ) -> Self {
        Self {
            run: run.to_string(),
            session: session.to_string(),
            generation: generation.into(),
            limit,
            native: position.cursor,
            file_offset: position.file_offset,
            ascending: position.ascending,
            skip_turn: position.skip_turn.map(|id| id.to_string()),
            within: position.within.map(|item| Within {
                turn: item.turn.to_string(),
                boundary: item.index,
                forward: item.forward,
            }),
        }
    }

    fn native_position(&self) -> Result<NativePosition, AppError> {
        let parse = |raw: &str| raw.parse().map_err(|_| AppError::CursorExpired);
        Ok(NativePosition {
            cursor: self.native.clone(),
            ascending: self.ascending,
            file_offset: self.file_offset,
            skip_turn: self.skip_turn.as_deref().map(parse).transpose()?,
            within: self
                .within
                .as_ref()
                .map(|w| {
                    Ok(TurnWindow {
                        turn: parse(&w.turn)?,
                        index: w.boundary,
                        forward: w.forward,
                    })
                })
                .transpose()?,
        })
    }
}
