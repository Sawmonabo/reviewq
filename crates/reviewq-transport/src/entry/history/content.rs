use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::history::{ContentReference, FileContentRef, NativePosition, RpcContentRef};
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_protocol::history::{
    HistoryAvailability, HistoryContent, HistoryContentResponse, HistoryText,
};
use reviewq_protocol::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, RunIdDto, SafeCountDto,
    SessionKeyDto,
};
use serde::{Deserialize, Serialize};

use super::{Entry, Point, ServiceEndpoint};
use crate::entry::diagrams::{self, Owner};
use crate::entry::error::{ApiError, ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{parse, success};

const TOKEN: &[u8] = b"reviewq-history-content-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::entry) struct ContentQuery {
    #[serde(rename = "ref")]
    pub(in crate::entry) reference: String,
    pub(in crate::entry) cursor: Option<String>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
struct Locator {
    run: String,
    session: String,
    generation: String,
    item: String,
    part: Option<u16>,
    position: Position,
    offset: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Position {
    File { point: Point },
    Rpc { turn: String, hash: String },
}

pub(in crate::entry) async fn content(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(input): Query<ContentQuery>,
) -> Result<Json<HistoryContentResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let source = input.reference.clone();
    let mut result = read_content(
        &entry,
        actor.clone(),
        parse(&run)?,
        parse(&session)?,
        input,
        64 * 1024,
    )
    .await?;
    if result.data.complete && result.data.offset.get() == 0 && !result.data.reset {
        result.data.display = diagrams::observe(
            &entry,
            &actor,
            Owner::Native {
                run,
                session,
                source,
            },
            result.data.text.as_str(),
        );
    }
    Ok(Json(result))
}

pub(in crate::entry) async fn read_content(
    entry: &Entry,
    actor: Actor,
    run: RunId,
    session: SessionKey,
    input: ContentQuery,
    limit: u32,
) -> Result<HistoryContentResponse, AppError> {
    let endpoint = &entry.auth.endpoint;
    let reference: Locator = endpoint.decode_cursor(TOKEN, &input.reference)?;
    if reference.run != run.as_str()
        || reference.session != session.as_str()
        || reference.offset != 0
    {
        return Err(AppError::CursorExpired);
    }
    let offset = if let Some(cursor) = input.cursor {
        let mut next: Locator = endpoint.decode_cursor(TOKEN, &cursor)?;
        let offset = next.offset;
        next.offset = 0;
        if next != reference {
            return Err(AppError::CursorExpired);
        }
        offset
    } else {
        0
    };
    let source = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let result = match &reference.position {
        Position::File { point } => {
            entry
                .file_history
                .content(
                    source.value.clone(),
                    FileContentRef {
                        generation: reference.generation.clone(),
                        entry: parse(&reference.item)?,
                        part: reference.part,
                        position: point.clone().into(),
                    },
                    offset,
                    limit,
                )
                .await
        }
        Position::Rpc { turn, hash } => {
            entry
                .host
                .as_ref()
                .ok_or(AppError::Unavailable)?
                .history_content(
                    source.value.clone(),
                    RpcContentRef {
                        generation: reference.generation.clone(),
                        entry: parse(&reference.item)?,
                        part: reference.part,
                        turn: parse(turn)?,
                        hash: parse(hash)?,
                    },
                    offset,
                    limit,
                )
                .await
        }
    };
    let (generation, text, next, reset) = match result {
        Ok(result) => (result.generation, result.text, result.next_offset, false),
        Err(AppError::CursorExpired) => {
            let current =
                super::native_page(entry, source.value.clone(), NativePosition::default(), 1)
                    .await?;
            (current.generation, String::new(), None, true)
        }
        Err(error) => return Err(error),
    };
    if entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?
        .value
        != source.value
    {
        return Err(AppError::CursorExpired);
    }
    let next_cursor = next
        .map(|offset| {
            let mut continuation = reference.clone();
            continuation.offset = offset;
            encode(endpoint, &continuation)
        })
        .transpose()?;
    success(
        HistoryContent {
            run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
            entry_id: HistoryEntryIdDto::new(reference.item).ok_or(AppError::Internal)?,
            generation: HistoryGenerationDto::new(generation).ok_or(AppError::Internal)?,
            availability: HistoryAvailability::Available,
            offset: SafeCountDto::new(offset).ok_or(AppError::Internal)?,
            text: HistoryText::new(text).ok_or(AppError::Internal)?,
            complete: next_cursor.is_none() && !reset,
            next_cursor,
            reset,
            display: None,
        },
        source.cursor,
    )
}

pub(super) fn reference(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    source: &ContentReference,
) -> Result<HistoryCursorDto, AppError> {
    let (generation, item, part, position) = match source {
        ContentReference::File(source) => (
            &source.generation,
            &source.entry,
            source.part,
            Position::File {
                point: source.position.clone().into(),
            },
        ),
        ContentReference::Rpc(source) => (
            &source.generation,
            &source.entry,
            source.part,
            Position::Rpc {
                turn: source.turn.to_string(),
                hash: source.hash.to_string(),
            },
        ),
    };
    encode(
        endpoint,
        &Locator {
            run: run.to_string(),
            session: session.to_string(),
            generation: generation.clone(),
            item: item.to_string(),
            part,
            position,
            offset: 0,
        },
    )
}

fn encode(endpoint: &ServiceEndpoint, locator: &Locator) -> Result<HistoryCursorDto, AppError> {
    HistoryCursorDto::new(endpoint.encode_cursor(TOKEN, locator)?).ok_or(AppError::Internal)
}
