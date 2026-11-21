use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::history::search::rpc::{
    RpcSearchEntry, RpcSearchPage, RpcSearchPoint, RpcSearchQuery,
};
use reviewq_app::history::search::{
    HistorySearch, SearchCursor, SearchHit, SearchPage, SearchPoint, SourceObservation,
};
use reviewq_app::history::{
    ContentReference, FileContentRef, HistoryBookmark, HistorySession, NativeRpcStamp,
    RpcContentRef,
};
use reviewq_app::reading::Literal;
use reviewq_domain::identity::{RunId, SessionKey};
use reviewq_protocol::history::search::{
    HistorySearchDestination, HistorySearchDestinationResponse, HistorySearchDocument,
    HistorySearchHit, HistorySearchPage, HistorySearchResponse,
};
use reviewq_protocol::history::{HistoryAvailability, HistoryText};
use reviewq_protocol::reports::ReportHashDto;
use reviewq_protocol::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, RunIdDto, SafeCountDto,
    SessionKeyDto,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Cursor, Entry, Point, ServiceEndpoint, content, explore, parts};
use crate::entry::error::{ApiError, ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{parse, success};

const PAGE_TOKEN: &[u8] = b"reviewq-history-search-page-v3";
const HIT_TOKEN: &[u8] = b"reviewq-history-search-hit-v3";

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
    position: Position,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
enum Position {
    File {
        high_water: u64,
        version: [u8; 32],
        record: u64,
        part: u16,
        window: Option<u64>,
        byte: u64,
    },
    Rpc {
        page: Box<Cursor>,
        tail: Option<String>,
        item: Option<RpcEntryAnchor>,
        part: u16,
        window: Option<u64>,
        byte: u64,
    },
}

#[derive(Serialize, Deserialize)]
struct RpcEntryAnchor {
    id: String,
    hash: String,
    page_size: u16,
}

impl RpcEntryAnchor {
    fn open(self) -> Result<RpcSearchEntry, AppError> {
        Ok(RpcSearchEntry {
            id: parse(&self.id)?,
            hash: parse(&self.hash)?,
            page_size: self.page_size,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Hit {
    target: Target,
    generation: String,
    entry: String,
    part: Option<u16>,
    source: HitSource,
    tool: Option<String>,
    offset: u64,
    #[serde(default)]
    output_bytes: Option<u64>,
    start: u64,
    end: u64,
    markdown: bool,
    document: Option<Document>,
    matched: String,
    ordinal: u64,
}

#[derive(Serialize, Deserialize)]
struct Document {
    sha256: String,
    byte_start: u64,
    canonical: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
enum HitSource {
    File {
        version: [u8; 32],
        point: Point,
    },
    Rpc {
        turn: String,
        hash: String,
        anchor: Box<Cursor>,
        tail: Option<String>,
    },
}

struct SearchScope<'a> {
    endpoint: &'a ServiceEndpoint,
    run: &'a RunId,
    session: &'a SessionKey,
    query: [u8; 32],
    reverse: bool,
}

impl SearchScope<'_> {
    fn cursor(&self, position: Position, reverse: bool) -> Result<HistoryCursorDto, AppError> {
        seal(
            self.endpoint,
            PAGE_TOKEN,
            &Continuation {
                target: Target::new(self.run, self.session),
                query: self.query,
                reverse,
                position,
            },
        )
    }

    fn rpc_position(&self, point: RpcSearchPoint, stamp: &NativeRpcStamp) -> Position {
        Position::Rpc {
            page: Box::new(self.rpc_anchor(point.page, &stamp.generation)),
            tail: stamp.tail.clone(),
            item: point.entry.map(|entry| RpcEntryAnchor {
                id: entry.id.to_string(),
                hash: entry.hash.to_string(),
                page_size: entry.page_size,
            }),
            part: point.part,
            window: point.window,
            byte: point.byte,
        }
    }

    fn rpc_anchor(&self, page: reviewq_app::history::NativePosition, generation: &str) -> Cursor {
        Cursor::new(self.run, self.session, generation, (32, 64 * 1024), page)
    }

    fn open(&self, input: &str) -> Result<Position, AppError> {
        let saved: Continuation = self.endpoint.decode_cursor(PAGE_TOKEN, input)?;
        saved.target.require(self.run, self.session)?;
        if saved.query != self.query || saved.reverse != self.reverse {
            return Err(AppError::CursorExpired);
        }
        Ok(saved.position)
    }

    fn page(
        &self,
        generation: Option<String>,
        availability: HistoryAvailability,
        hits: Vec<HistorySearchHit>,
        next_cursor: Option<HistoryCursorDto>,
    ) -> Result<HistorySearchPage, AppError> {
        Ok(HistorySearchPage {
            run_id: RunIdDto::new(self.run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(self.session.as_str()).ok_or(AppError::Internal)?,
            generation: generation
                .map(|value| HistoryGenerationDto::new(value).ok_or(AppError::Internal))
                .transpose()?,
            availability,
            hits,
            next_cursor,
        })
    }

    fn hit<R, P>(
        &self,
        found: SearchHit<R, P>,
        origin: HitIdentity,
        cursors: (HistoryCursorDto, HistoryCursorDto),
    ) -> Result<HistorySearchHit, AppError> {
        let signed = Hit {
            target: Target::new(self.run, self.session),
            generation: origin.generation,
            entry: origin.entry,
            part: origin.part,
            source: origin.source,
            tool: found.tool_call.map(|call| call.to_string()),
            offset: found.output_offset,
            output_bytes: Some(found.output_bytes),
            start: found.start_utf16,
            end: found.end_utf16,
            markdown: found.markdown,
            document: found.document.map(|proof| Document {
                sha256: proof.hash.to_string(),
                byte_start: proof.byte_start,
                canonical: proof.canonical,
            }),
            matched: found.matched.clone(),
            ordinal: found.ordinal,
        };
        Ok(HistorySearchHit {
            hit: seal(self.endpoint, HIT_TOKEN, &signed)?,
            entry_id: HistoryEntryIdDto::new(signed.entry).ok_or(AppError::Internal)?,
            before: text(found.before)?,
            matched: text(found.matched)?,
            after: text(found.after)?,
            before_cursor: cursors.0,
            after_cursor: cursors.1,
        })
    }

    fn file(&self, page: SearchPage) -> Result<HistorySearchPage, AppError> {
        let wire = |point: SearchPoint, reverse| {
            self.cursor(
                Position::File {
                    high_water: page.observation.high_water,
                    version: page.observation.version,
                    record: point.record,
                    part: point.part,
                    window: point.window,
                    byte: point.byte,
                },
                reverse,
            )
        };
        let hits = page
            .hits
            .into_iter()
            .map(|found| {
                let reference = &found.reference;
                let origin = HitIdentity {
                    generation: reference.generation.clone(),
                    entry: reference.entry.to_string(),
                    part: reference.part,
                    source: HitSource::File {
                        version: page.observation.version,
                        point: reference.position.clone().into(),
                    },
                };
                let point = found.point;
                let cursors = (
                    wire(point, true)?,
                    wire(
                        SearchPoint {
                            byte: point.byte + 1,
                            ..point
                        },
                        false,
                    )?,
                );
                self.hit(found, origin, cursors)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.page(
            Some(page.generation),
            availability(page.partial),
            hits,
            page.next
                .map(|point| wire(point, self.reverse))
                .transpose()?,
        )
    }

    fn rpc(&self, page: RpcSearchPage) -> Result<HistorySearchPage, AppError> {
        let stamp = &page.observation;
        let wire =
            |point: RpcSearchPoint, reverse| self.cursor(self.rpc_position(point, stamp), reverse);
        let hits = page
            .hits
            .into_iter()
            .map(|found| {
                let reference = &found.reference;
                let origin = HitIdentity {
                    generation: reference.generation.clone(),
                    entry: reference.entry.to_string(),
                    part: reference.part,
                    source: HitSource::Rpc {
                        turn: reference.turn.to_string(),
                        hash: reference.hash.to_string(),
                        anchor: Box::new(
                            self.rpc_anchor(found.point.page.clone(), &reference.generation),
                        ),
                        tail: stamp.tail.clone(),
                    },
                };
                let before = found.point.clone();
                let after = RpcSearchPoint {
                    byte: found.point.byte + 1,
                    ..found.point.clone()
                };
                self.hit(found, origin, (wire(before, true)?, wire(after, false)?))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.page(
            Some(stamp.generation.clone()),
            availability(page.partial),
            hits,
            page.next
                .map(|point| wire(point, self.reverse))
                .transpose()?,
        )
    }
}

struct HitIdentity {
    generation: String,
    entry: String,
    part: Option<u16>,
    source: HitSource,
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
    let scope = SearchScope {
        endpoint: &entry.auth.endpoint,
        run: &run,
        session: &session,
        query: Sha256::digest(literal.folded()).into(),
        reverse: input.reverse,
    };
    let from = input
        .cursor
        .as_deref()
        .map(|token| scope.open(token))
        .transpose()?;
    let current = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let page = read_authorized(&entry, &scope, current.value.clone(), literal, from).await?;
    let confirmed = entry.store.history_session(actor, run, session).await?;
    if current.value != confirmed.value {
        return Err(AppError::CursorExpired.into());
    }
    Ok(Json(success(page, current.cursor)?))
}

async fn read_authorized(
    entry: &Entry,
    scope: &SearchScope<'_>,
    target: HistorySession,
    literal: Literal,
    from: Option<Position>,
) -> Result<HistorySearchPage, AppError> {
    let reverse = scope.reverse;
    let rpc = match from {
        Some(Position::File {
            high_water,
            version,
            record,
            part,
            window,
            byte,
        }) => {
            let from = Some(SearchCursor {
                observation: SourceObservation {
                    high_water,
                    version,
                },
                point: SearchPoint {
                    record,
                    part,
                    window,
                    byte,
                },
            });
            let page = entry
                .file_history
                .search(
                    target,
                    HistorySearch {
                        literal,
                        from,
                        reverse,
                    },
                )
                .await?
                .ok_or(AppError::CursorExpired)?;
            return scope.file(page);
        }
        Some(Position::Rpc {
            page,
            tail,
            item,
            part,
            window,
            byte,
        }) => {
            if entry
                .file_history
                .head(target.clone(), None)
                .await?
                .is_some()
            {
                return Err(AppError::CursorExpired);
            }
            Some((
                NativeRpcStamp {
                    generation: page.generation.clone(),
                    tail,
                },
                RpcSearchPoint {
                    page: page.native_position()?,
                    entry: item.map(RpcEntryAnchor::open).transpose()?,
                    part,
                    window,
                    byte,
                },
            ))
        }
        None => {
            if let Some(page) = entry
                .file_history
                .search(
                    target.clone(),
                    HistorySearch {
                        literal: literal.clone(),
                        from: None,
                        reverse,
                    },
                )
                .await?
            {
                return scope.file(page);
            }
            None
        }
    };
    let from_rpc = rpc.is_some();
    if let Some(host) = entry.host.as_ref() {
        match host
            .history_search(
                target,
                RpcSearchQuery {
                    literal,
                    from: rpc,
                    reverse,
                },
            )
            .await
        {
            Ok(page) => return scope.rpc(page),
            Err(AppError::Unavailable | AppError::NotFound) if !from_rpc => (),
            Err(error) => return Err(error),
        }
    }
    if from_rpc {
        return Err(AppError::CursorExpired);
    }
    scope.page(None, HistoryAvailability::Unavailable, Vec::new(), None)
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
    let current = entry
        .store
        .history_session(actor.clone(), run.clone(), session.clone())
        .await?;
    let (source, bookmark) = verify_hit(&entry, current.value.clone(), &hit).await?;
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
        &hit.generation,
        &parse(&hit.entry)?,
        &bookmark,
    )?;
    let parts_cursor = source
        .file()
        .and_then(|reference| reference.part.map(|index| (reference, index)))
        .map(|(reference, part)| {
            let mut entry = reference.clone();
            entry.part = None;
            parts::cursor(endpoint, &run, &session, &entry, part)
        })
        .transpose()?;
    let content_ref = content::reference(endpoint, &run, &session, &source)?;
    let content_cursor = (hit.offset > 0)
        .then(|| content::ranged_reference(endpoint, &run, &session, &source, hit.offset))
        .transpose()?;
    Ok(Json(success(
        HistorySearchDestination {
            run_id: RunIdDto::new(run.as_str()).ok_or(AppError::Internal)?,
            session_key: SessionKeyDto::new(session.as_str()).ok_or(AppError::Internal)?,
            entry_id: HistoryEntryIdDto::new(hit.entry).ok_or(AppError::Internal)?,
            generation: HistoryGenerationDto::new(hit.generation).ok_or(AppError::Internal)?,
            locator,
            content_ref,
            content_cursor,
            parts_cursor,
            native_index: hit.part.map(|index| count(u64::from(index))).transpose()?,
            tool_call_id: hit
                .tool
                .map(|tool| HistoryEntryIdDto::new(tool).ok_or(AppError::Internal))
                .transpose()?,
            offset: count(hit.offset)?,
            output_bytes: hit.output_bytes.map(count).transpose()?,
            document: hit
                .document
                .map(|doc| {
                    Ok::<_, AppError>(HistorySearchDocument {
                        sha256: ReportHashDto::new(doc.sha256).ok_or(AppError::Internal)?,
                        byte_start: count(doc.byte_start)?,
                        canonical: doc.canonical,
                    })
                })
                .transpose()?,
            start_utf16: count(hit.start)?,
            end_utf16: count(hit.end)?,
            markdown: hit.markdown,
            matched: text(hit.matched)?,
            ordinal: count(hit.ordinal)?,
        },
        current.cursor,
    )?))
}

async fn verify_hit(
    entry: &Entry,
    target: HistorySession,
    hit: &Hit,
) -> Result<(ContentReference, HistoryBookmark), AppError> {
    match &hit.source {
        HitSource::File { version, point } => {
            let observation = SourceObservation {
                high_water: point.high_water,
                version: *version,
            };
            let reference = FileContentRef {
                generation: hit.generation.clone(),
                entry: parse(&hit.entry)?,
                part: hit.part,
                position: point.clone().into(),
            };
            entry
                .file_history
                .confirm_search_hit(target, observation, reference.clone())
                .await?;
            let bookmark = HistoryBookmark::File(reference.position.clone());
            Ok((ContentReference::File(reference), bookmark))
        }
        HitSource::Rpc {
            turn,
            hash,
            anchor,
            tail,
        } => {
            if entry
                .file_history
                .head(target.clone(), None)
                .await?
                .is_some()
            {
                return Err(AppError::CursorExpired);
            }
            let host = entry.host.as_ref().ok_or(AppError::Unavailable)?;
            let expected = NativeRpcStamp {
                generation: hit.generation.clone(),
                tail: tail.clone(),
            };
            if host.history_search_stamp(target.clone()).await? != expected {
                return Err(AppError::CursorExpired);
            }
            let reference = RpcContentRef {
                generation: hit.generation.clone(),
                entry: parse(&hit.entry)?,
                part: hit.part,
                turn: parse(turn)?,
                hash: parse(hash)?,
            };
            host.history_content(target.clone(), reference.clone(), hit.offset, 4)
                .await?;
            if host.history_search_stamp(target).await? != expected {
                return Err(AppError::CursorExpired);
            }
            Ok((
                ContentReference::Rpc(reference),
                HistoryBookmark::Rpc(anchor.native_position()?),
            ))
        }
    }
}

fn availability(partial: bool) -> HistoryAvailability {
    if partial {
        HistoryAvailability::Partial
    } else {
        HistoryAvailability::Available
    }
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
