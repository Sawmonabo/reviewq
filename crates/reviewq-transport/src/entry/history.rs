use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::{
    FilePosition, HistorySession, NativeHistoryPage, NativePosition, ToolState, TurnWindow,
    VisibleEntry, VisibleKind, VisiblePart, VisiblePartKind,
};
use reviewq_domain::identity::{EventCursor, NativeInputId, RunId, SessionKey};
use reviewq_protocol::history::{
    Direction, HistoryAvailability, HistoryEntry, HistoryPage, HistoryPageEncodeError,
    HistoryPageResponse, HistoryPart, HistoryText, HistoryToolState, HistoryWindow,
};
use reviewq_protocol::{
    EventCursorDto, HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, HistoryTurnIdDto,
    RunIdDto, SafeCountDto, SessionKeyDto, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::error::{ApiPath as Path, ApiQuery as Query};
use super::{ApiError, Entry, ServiceEndpoint};

const CURSOR: &[u8] = b"reviewq-native-history-v1";
const DEFAULT_PAGE_LIMIT: u16 = 30;
const DEFAULT_PAGE_BYTES: usize = 64 * 1024;

mod content;
mod explore;
pub(super) use content::{ContentQuery, content, read_content};
mod live;
mod parts;
pub(super) use live::Watchers;

pub(super) fn routes() -> axum::Router<Arc<Entry>> {
    use axum::routing::get;
    axum::Router::new()
        .route(
            "/runs/{run_id}/sessions/{session_key}/history/parts",
            get(parts::read),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/history/navigation",
            get(explore::navigation),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/history/plan",
            get(explore::plan),
        )
        .route(
            "/runs/{run_id}/sessions/{session_key}/history/stream",
            get(live::live),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct HistoryQuery {
    pub(super) limit: Option<u16>,
    pub(super) cursor: Option<String>,
    pub(super) around_id: Option<String>,
    pub(super) locator: Option<String>,
    pub(super) direction: Option<Direction>,
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
    file: Option<Point>,
    byte_limit: usize,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
struct Point {
    offset: u64,
    high_water: u64,
    witness: [u8; 32],
}

impl From<FilePosition> for Point {
    fn from(point: FilePosition) -> Self {
        Self {
            offset: point.offset,
            high_water: point.high_water,
            witness: point.witness,
        }
    }
}

impl From<Point> for FilePosition {
    fn from(point: Point) -> Self {
        Self {
            offset: point.offset,
            high_water: point.high_water,
            witness: point.witness,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Within {
    turn: String,
    forward: bool,
    item_cursor: Option<String>,
    item_anchor: Option<String>,
}

pub(super) async fn page(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let budget = if query.limit.is_some() {
        256 * 1024
    } else {
        DEFAULT_PAGE_BYTES
    };
    let body = read(
        &entry,
        actor,
        super::pages::parse(&run)?,
        super::pages::parse(&session)?,
        query,
        budget,
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
    byte_limit: usize,
) -> Result<Vec<u8>, AppError> {
    let limit = query.limit.unwrap_or(DEFAULT_PAGE_LIMIT);
    if !(1..=100).contains(&limit) {
        return Err(AppError::InvalidRequest);
    }
    let around: Option<NativeInputId> = query
        .around_id
        .as_deref()
        .map(super::pages::parse)
        .transpose()?;
    if (around.is_some() && (query.cursor.is_some() || query.direction.is_some()))
        || (query.locator.is_some() && around.is_none())
    {
        return Err(AppError::InvalidRequest);
    }
    let endpoint = &entry.auth.endpoint;
    let cursor = query
        .cursor
        .as_deref()
        .map(|token| endpoint.decode_cursor::<Cursor>(CURSOR, token))
        .transpose()?;
    let requested_newer = query
        .direction
        .map(|value| matches!(value, Direction::Newer));
    let cursor_newer = cursor.as_ref().map(|cursor| {
        cursor
            .within
            .as_ref()
            .map_or(cursor.ascending, |within| within.forward)
    });
    if cursor.as_ref().is_some_and(|cursor| {
        cursor.run != run.as_str()
            || cursor.session != session.as_str()
            || cursor.limit != limit
            || cursor.byte_limit != byte_limit
            || requested_newer.is_some_and(|value| Some(value) != cursor_newer)
    }) {
        return Err(AppError::CursorExpired);
    }
    let lead = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let newer = requested_newer.or(cursor_newer).unwrap_or(false);
    let first = NativePosition {
        ascending: newer,
        ..NativePosition::default()
    };
    let position = cursor
        .as_ref()
        .map(Cursor::native_position)
        .transpose()?
        .unwrap_or_else(|| first.clone());
    if let Some(around) = around {
        let anchor = query
            .locator
            .as_deref()
            .map(|token| explore::open_anchor(endpoint, &run, &session, &around, token))
            .transpose()?;
        let current = around_page(entry, lead.value.clone(), &around, anchor.clone(), limit).await;
        let (mut result, reset) = match current {
            Ok(current) => (current, false),
            Err(AppError::CursorExpired) if anchor.is_some() => (
                around_page(entry, lead.value.clone(), &around, None, limit).await?,
                true,
            ),
            Err(error) => return Err(error),
        };
        bind_observed_turns(entry, actor, lead.value, &mut result).await?;
        return encode(
            endpoint,
            &run,
            &session,
            lead.cursor,
            (limit, byte_limit),
            result,
            reset,
            Trim::Around(&around),
        );
    }
    let mut reset = false;
    let history = native_page(entry, lead.value.clone(), position, limit).await;
    let mut history = match history {
        Ok(page)
            if cursor
                .as_ref()
                .is_none_or(|cursor| cursor.generation == page.generation) =>
        {
            page
        }
        Ok(_) | Err(AppError::CursorExpired) if cursor.is_some() => {
            reset = true;
            native_page(entry, lead.value.clone(), first, limit).await?
        }
        Err(error) => return Err(error),
        Ok(page) => page,
    };
    bind_observed_turns(entry, actor, lead.value, &mut history).await?;
    encode(
        endpoint,
        &run,
        &session,
        lead.cursor,
        (limit, byte_limit),
        history,
        reset,
        if newer {
            Trim::FromStart
        } else {
            Trim::FromEnd
        },
    )
}

async fn bind_observed_turns(
    entry: &Entry,
    actor: Actor,
    source: HistorySession,
    page: &mut NativeHistoryPage,
) -> Result<(), AppError> {
    let inputs = page
        .entries
        .iter()
        .filter(|item| item.kind == VisibleKind::User && item.turn.is_none())
        .map(|item| item.id.clone())
        .collect::<Vec<_>>();
    if inputs.is_empty() {
        let current = entry
            .store
            .history_session(
                actor,
                source.target.run_id().clone(),
                source.target.session_key().clone(),
            )
            .await?;
        if current.value != source {
            return Err(AppError::CursorExpired);
        }
        return Ok(());
    }
    let known = entry
        .store
        .verified_history_turns(actor, source, inputs)
        .await?;
    for item in &mut page.entries {
        if item.kind == VisibleKind::User && item.turn.is_none() {
            item.turn = known.get(&item.id).cloned();
        }
    }
    Ok(())
}

async fn around_page(
    entry: &Entry,
    session: HistorySession,
    id: &NativeInputId,
    anchor: Option<(String, reviewq_app::history::HistoryBookmark)>,
    limit: u16,
) -> Result<NativeHistoryPage, AppError> {
    use reviewq_app::history::{HistoryBookmark, SourceAnchor};
    if let Some((generation, HistoryBookmark::Rpc(position))) = anchor.as_ref() {
        if entry
            .file_history
            .head(session.clone(), None)
            .await?
            .is_some()
        {
            return Err(AppError::CursorExpired);
        }
        return entry
            .host
            .as_ref()
            .ok_or(AppError::Unavailable)?
            .history_around(
                session,
                id.clone(),
                Some((position.clone(), generation.clone())),
                limit,
            )
            .await;
    }
    let file = anchor
        .map(|(generation, bookmark)| {
            Ok(SourceAnchor {
                generation,
                file: bookmark.into_file()?,
            })
        })
        .transpose()?;
    if let Some(result) = entry
        .file_history
        .around(session.clone(), id.clone(), file.clone(), limit)
        .await?
    {
        return Ok(result);
    }
    if file.is_some() {
        return Err(AppError::CursorExpired);
    }
    entry
        .host
        .as_ref()
        .ok_or(AppError::Unavailable)?
        .history_around(session, id.clone(), None, limit)
        .await
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

enum Trim<'a> {
    FromStart,
    FromEnd,
    Around(&'a NativeInputId),
}

fn encode(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    event: EventCursor,
    budget: (u16, usize),
    history: NativeHistoryPage,
    reset: bool,
    trim: Trim<'_>,
) -> Result<Vec<u8>, AppError> {
    let cursor = |position: Option<NativePosition>| {
        position
            .map(|position| {
                let witness = Cursor::new(run, session, &history.generation, budget, position);
                HistoryCursorDto::new(endpoint.encode_cursor(CURSOR, &witness)?)
                    .ok_or(AppError::Internal)
            })
            .transpose()
    };
    let mut first = 0;
    let mut last = history.entries.len();
    let mut part_limit = usize::MAX;
    let pinned = match trim {
        Trim::Around(id) => history.entries.iter().position(|entry| &entry.id == id),
        _ => None,
    };
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
                    .skip(first)
                    .take(last - first)
                    .map(|entry| encode_entry(endpoint, run, session, entry, part_limit))
                    .collect::<Result<Vec<_>, _>>()?,
                cursor(history.older_after_trim(first))?,
                cursor(history.newer_after_trim(last))?,
            )
            .ok_or(AppError::Internal)?,
        };
        let result = HistoryPageResponse::new(
            data,
            EventCursorDto::new(event.get()).ok_or(AppError::Internal)?,
        )
        .encode_bounded(budget.1);
        match result {
            Ok(bytes) => return Ok(bytes),
            Err(HistoryPageEncodeError::TooLarge) if last - first > 1 => {
                let remove = (last - first).div_ceil(4);
                if let Some(anchor) = pinned
                    && last - anchor - 1 > anchor - first
                {
                    last -= remove.min(last - anchor - 1);
                } else if let Some(anchor) = pinned {
                    first += remove.min(anchor - first);
                } else if matches!(trim, Trim::FromStart) {
                    last -= remove.min(last - first - 1);
                } else {
                    first += remove.min(last - first - 1);
                }
            }
            Err(HistoryPageEncodeError::TooLarge) => {
                let count = history
                    .entries
                    .get(first)
                    .filter(|entry| {
                        entry
                            .content
                            .as_ref()
                            .and_then(reviewq_app::history::ContentReference::file)
                            .is_some()
                    })
                    .map_or(0, |entry| entry.parts.len().min(part_limit));
                if count <= 1 {
                    return Err(AppError::PayloadTooLarge);
                }
                part_limit = count.div_ceil(2);
            }
            Err(HistoryPageEncodeError::Serialization(_)) => return Err(AppError::Internal),
        }
    }
}

fn encode_entry(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    entry: &VisibleEntry,
    part_limit: usize,
) -> Result<HistoryEntry, AppError> {
    let end = entry
        .text
        .char_indices()
        .nth(512)
        .map_or(entry.text.len(), |(offset, _)| offset);
    let text = HistoryText::new(&entry.text[..end]).ok_or(AppError::Internal)?;
    let part = match entry.kind {
        VisibleKind::User => HistoryPart::UserPrompt {
            text,
            content_ref: None,
        },
        VisibleKind::Assistant => HistoryPart::AssistantText {
            text,
            content_ref: None,
        },
    };
    let next = entry
        .parts
        .get(part_limit)
        .map(|part| part.native_index)
        .or(entry.next_part);
    let parts_cursor = match next {
        Some(first) => {
            let file = entry
                .content
                .as_ref()
                .and_then(reviewq_app::history::ContentReference::file)
                .ok_or(AppError::Internal)?;
            Some(parts::cursor(endpoint, run, session, file, first)?)
        }
        None => None,
    };
    Ok(HistoryEntry {
        parts_cursor,
        entry_id: HistoryEntryIdDto::new(entry.id.as_str()).ok_or(AppError::Internal)?,
        turn_id: entry
            .turn
            .as_ref()
            .map(|turn| HistoryTurnIdDto::new(turn.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        timestamp: entry
            .at_ms
            .and_then(UtcTimestampDto::from_unix_milliseconds),
        parts: if entry.parts.is_empty() {
            vec![part]
        } else {
            entry
                .parts
                .iter()
                .take(part_limit)
                .map(|part| visible_part(endpoint, run, session, part))
                .collect::<Result<_, _>>()?
        },
        content_ref: entry
            .content
            .as_ref()
            .filter(|_| entry.text_bytes.is_some_and(|count| count > end as u64))
            .map(|source| content::reference(endpoint, run, session, source))
            .transpose()?,
        text_bytes: entry.text_bytes.and_then(SafeCountDto::new),
    })
}

fn visible_part(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    part: &VisiblePart,
) -> Result<HistoryPart, AppError> {
    let text = HistoryText::new(part.text.as_str()).ok_or(AppError::Internal)?;
    let content_ref = part
        .content
        .as_ref()
        .map(|source| content::reference(endpoint, run, session, source))
        .transpose()?;
    let tool_state = |value| match value {
        ToolState::Running => HistoryToolState::Running,
        ToolState::Completed => HistoryToolState::Completed,
        ToolState::Failed => HistoryToolState::Failed,
        ToolState::Unknown => HistoryToolState::Unknown,
    };
    Ok(match &part.kind {
        VisiblePartKind::Text(VisibleKind::User) => HistoryPart::UserPrompt { text, content_ref },
        VisiblePartKind::Text(VisibleKind::Assistant) => {
            HistoryPart::AssistantText { text, content_ref }
        }
        VisiblePartKind::ToolCall {
            call,
            name,
            label,
            state,
        } => HistoryPart::ToolCall {
            call_id: HistoryEntryIdDto::new(call.as_str()).ok_or(AppError::Internal)?,
            name: name.clone(),
            label,
            state: tool_state(*state),
            text,
            content_ref,
        },
        VisiblePartKind::ToolResult { call, state } => HistoryPart::ToolResult {
            call_id: HistoryEntryIdDto::new(call.as_str()).ok_or(AppError::Internal)?,
            state: tool_state(*state),
            text,
            content_ref,
        },
        VisiblePartKind::Plan => HistoryPart::Plan { text, content_ref },
        VisiblePartKind::Assignment => HistoryPart::Assignment { text, content_ref },
    })
}

impl Cursor {
    fn new(
        run: &RunId,
        session: &SessionKey,
        generation: &str,
        budget: (u16, usize),
        position: NativePosition,
    ) -> Self {
        Self {
            run: run.to_string(),
            session: session.to_string(),
            generation: generation.into(),
            limit: budget.0,
            byte_limit: budget.1,
            native: position.cursor,
            file: position.file.map(Into::into),
            ascending: position.ascending,
            skip_turn: position.skip_turn.map(|id| id.to_string()),
            within: position.within.map(|item| Within {
                turn: item.turn.to_string(),
                forward: item.forward,
                item_cursor: item.item_cursor,
                item_anchor: item.item_anchor.map(|id| id.to_string()),
            }),
        }
    }

    fn native_position(&self) -> Result<NativePosition, AppError> {
        let parse = |raw: &str| raw.parse().map_err(|_| AppError::CursorExpired);
        Ok(NativePosition {
            cursor: self.native.clone(),
            ascending: self.ascending,
            file: self.file.clone().map(Into::into),
            skip_turn: self.skip_turn.as_deref().map(parse).transpose()?,
            within: self
                .within
                .as_ref()
                .map(|w| {
                    Ok(TurnWindow {
                        turn: parse(&w.turn)?,
                        forward: w.forward,
                        item_cursor: w.item_cursor.clone(),
                        item_anchor: w
                            .item_anchor
                            .as_deref()
                            .map(|id| id.parse().map_err(|_| AppError::CursorExpired))
                            .transpose()?,
                    })
                })
                .transpose()?,
        })
    }
}
