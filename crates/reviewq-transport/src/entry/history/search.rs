use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::history::search::{
    HistorySearch, SearchCursor, SearchHit, SearchPoint, SourceObservation,
};
use reviewq_app::history::{ContentReference, FileContentRef, HistoryBookmark};
use reviewq_app::reading::Literal;
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_protocol::history::search::{
    HistorySearchDestination, HistorySearchDestinationResponse, HistorySearchHit,
    HistorySearchPage, HistorySearchResponse,
};
use reviewq_protocol::history::{HistoryAvailability, HistoryText};
use reviewq_protocol::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, RunIdDto, SafeCountDto,
    SessionKeyDto,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Entry, Point, ServiceEndpoint, content, explore, parts};
use crate::entry::error::{ApiError, ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{parse, success};

const PAGE_TOKEN: &[u8] = b"reviewq-history-search-page-v1";
const HIT_TOKEN: &[u8] = b"reviewq-history-search-hit-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchQuery {
    q: String,
    cursor: Option<String>,
    #[serde(default)]
    reverse: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LocateQuery {
    hit: String,
}

#[derive(Serialize, Deserialize)]
struct Target {
    run: String,
    session: String,
}

impl Target {
    fn new(run: &RunId, session: &SessionKey) -> Self {
        Self {
            run: run.to_string(),
            session: session.to_string(),
        }
    }

    fn require(&self, run: &RunId, session: &SessionKey) -> Result<(), AppError> {
        if self.run != run.as_str() || self.session != session.as_str() {
            return Err(AppError::CursorExpired);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Continuation {
    target: Target,
    query: [u8; 32],
    reverse: bool,
    high_water: u64,
    version: [u8; 32],
    record: u64,
    part: u16,
    byte: u64,
}

#[derive(Serialize, Deserialize)]
struct Hit {
    target: Target,
    version: [u8; 32],
    generation: String,
    entry: String,
    part: Option<u16>,
    point: Point,
    tool: Option<String>,
    offset: u64,
    start: u64,
    end: u64,
    markdown: bool,
    matched: String,
    ordinal: u64,
}

pub(super) async fn read(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(input): Query<SearchQuery>,
) -> Result<Json<HistorySearchResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let literal = Literal::new(&input.q).ok_or(AppError::InvalidRequest)?;
    let query: [u8; 32] = Sha256::digest(literal.folded()).into();
    let endpoint = &entry.auth.endpoint;
    let from = input
        .cursor
        .map(|value| {
            let saved: Continuation = endpoint.decode_cursor(PAGE_TOKEN, &value)?;
            saved.target.require(&run, &session)?;
            if saved.query != query || saved.reverse != input.reverse {
                return Err(AppError::CursorExpired);
            }
            Ok(SearchCursor {
                observation: SourceObservation {
                    high_water: saved.high_water,
                    version: saved.version,
                },
                point: SearchPoint {
                    record: saved.record,
                    part: saved.part,
                    byte: saved.byte,
                },
            })
        })
        .transpose()?;
    let current = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let page = entry
        .file_history
        .search(
            current.value.clone(),
            HistorySearch {
                literal,
                from,
                reverse: input.reverse,
            },
        )
        .await?;
    let confirmed = entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?;
    if current.value != confirmed.value {
        return Err(AppError::CursorExpired.into());
    }
    let (generation, availability, hits, next_cursor) = if let Some(page) = page {
        let cursor = |point: SearchPoint, reverse| {
            seal(
                endpoint,
                PAGE_TOKEN,
                &Continuation {
                    target: Target::new(&run, &session),
                    query,
                    reverse,
                    high_water: page.observation.high_water,
                    version: page.observation.version,
                    record: point.record,
                    part: point.part,
                    byte: point.byte,
                },
            )
        };
        let hits = page
            .hits
            .into_iter()
            .map(|hit| encode_hit(endpoint, &run, &session, &page.observation, hit, &cursor))
            .collect::<Result<Vec<_>, _>>()?;
        (
            Some(HistoryGenerationDto::new(page.generation).ok_or(AppError::Internal)?),
            if page.partial {
                HistoryAvailability::Partial
            } else {
                HistoryAvailability::Available
            },
            hits,
            page.next
                .map(|next| cursor(next, input.reverse))
                .transpose()?,
        )
    } else {
        (None, HistoryAvailability::Unavailable, Vec::new(), None)
    };
    Ok(Json(success(
        HistorySearchPage {
            run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
            generation,
            availability,
            hits,
            next_cursor,
        },
        current.cursor,
    )?))
}

pub(super) async fn locate(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path((run, session)): Path<(String, String)>,
    Query(input): Query<LocateQuery>,
) -> Result<Json<HistorySearchDestinationResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let run: RunId = parse(&run)?;
    let session: SessionKey = parse(&session)?;
    let endpoint = &entry.auth.endpoint;
    let hit: Hit = endpoint.decode_cursor(HIT_TOKEN, &input.hit)?;
    hit.target.require(&run, &session)?;
    let observation = SourceObservation {
        high_water: hit.point.high_water,
        version: hit.version,
    };
    let reference = FileContentRef {
        generation: hit.generation,
        entry: parse(&hit.entry)?,
        part: hit.part,
        position: hit.point.into(),
    };
    let current = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    entry
        .file_history
        .confirm_search_hit(current.value.clone(), observation, reference.clone())
        .await?;
    let confirmed = entry
        .store
        .history_session(actor, run.clone(), session.clone())
        .await?;
    if current.value != confirmed.value {
        return Err(AppError::CursorExpired.into());
    }
    let locator = explore::entry_locator(
        endpoint,
        &run,
        &session,
        &reference.generation,
        &reference.entry,
        &HistoryBookmark::File(reference.position.clone()),
    )?;
    let parts_cursor = reference
        .part
        .map(|part| {
            let mut entry = reference.clone();
            entry.part = None;
            parts::cursor(endpoint, &run, &session, &entry, part)
        })
        .transpose()?;
    let source = ContentReference::File(reference);
    let content_ref = content::reference(endpoint, &run, &session, &source)?;
    let content_cursor = (hit.offset > 0)
        .then(|| content::ranged_reference(endpoint, &run, &session, &source, hit.offset))
        .transpose()?;
    let reference = source.file().ok_or(AppError::Internal)?;
    Ok(Json(success(
        HistorySearchDestination {
            run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
            entry_id: HistoryEntryIdDto::new(hit.entry).ok_or(AppError::Internal)?,
            generation: HistoryGenerationDto::new(reference.generation.as_str())
                .ok_or(AppError::Internal)?,
            locator,
            content_ref,
            content_cursor,
            parts_cursor,
            native_index: reference
                .part
                .map(|index| count(u64::from(index)))
                .transpose()?,
            tool_call_id: hit
                .tool
                .map(|tool| HistoryEntryIdDto::new(tool).ok_or(AppError::Internal))
                .transpose()?,
            offset: count(hit.offset)?,
            start_utf16: count(hit.start)?,
            end_utf16: count(hit.end)?,
            markdown: hit.markdown,
            matched: text(hit.matched)?,
            ordinal: count(hit.ordinal)?,
        },
        current.cursor,
    )?))
}

fn encode_hit(
    endpoint: &ServiceEndpoint,
    run: &RunId,
    session: &SessionKey,
    observation: &SourceObservation,
    found: SearchHit,
    cursor: &impl Fn(SearchPoint, bool) -> Result<HistoryCursorDto, AppError>,
) -> Result<HistorySearchHit, AppError> {
    let signed = Hit {
        target: Target::new(run, session),
        version: observation.version,
        generation: found.reference.generation,
        entry: found.reference.entry.to_string(),
        part: found.reference.part,
        point: found.reference.position.into(),
        tool: found.tool_call.map(|call| call.to_string()),
        offset: found.output_offset,
        start: found.start_utf16,
        end: found.end_utf16,
        markdown: found.markdown,
        matched: found.matched.clone(),
        ordinal: found.ordinal,
    };
    Ok(HistorySearchHit {
        hit: seal(endpoint, HIT_TOKEN, &signed)?,
        entry_id: HistoryEntryIdDto::new(signed.entry).ok_or(AppError::Internal)?,
        before: text(found.before)?,
        matched: text(found.matched)?,
        after: text(found.after)?,
        before_cursor: cursor(found.point, true)?,
        after_cursor: cursor(
            SearchPoint {
                byte: found.point.byte + 1,
                ..found.point
            },
            false,
        )?,
    })
}

fn seal(
    endpoint: &ServiceEndpoint,
    purpose: &[u8],
    value: &impl Serialize,
) -> Result<HistoryCursorDto, AppError> {
    HistoryCursorDto::new(endpoint.encode_cursor(purpose, value)?).ok_or(AppError::Internal)
}

fn count(count: u64) -> Result<SafeCountDto, AppError> {
    SafeCountDto::new(count).ok_or(AppError::Internal)
}

fn text(text: String) -> Result<HistoryText, AppError> {
    HistoryText::new(text).ok_or(AppError::Internal)
}
