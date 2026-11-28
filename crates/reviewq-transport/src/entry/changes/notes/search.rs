use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::changes::{
    DiscussionAnchor, DiscussionInvalidation, DiscussionPage, DiscussionPageRequest,
    NoteOccurrence, discussion_page_at, note_occurrence,
};
use reviewq_app::reading::{FoldedText, Literal, displayed_markdown};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use super::super::{DISCUSSION_LIMIT, loses_access};
use super::{
    Address, ApiError, Entry, HIT, Hit, Path, Query, ReviewIdDto, Scope, Success, Witness,
    check_discussions, count, dto, event_cursor, note_kind, parse_review, patch_revision,
    read_source, token, valid_revision,
};

const CURSOR: &[u8] = b"reviewq-changes-note-search-v1";
const MAX_HITS: usize = 16;
const MAX_SCAN_BYTES: usize = 256 * 1024;
static SCAN_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::entry) struct SearchQuery {
    revision: String,
    query: String,
    cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct Position {
    scope: Scope,
    query: [u8; 32],
    after: After,
    limited: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum After {
    Published { page: u32, limit: u16, point: Point },
    Drafts { point: Point },
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
struct Point {
    note: usize,
    byte: usize,
}

struct Candidate<'a> {
    thread_id: Option<&'a str>,
    note_id: u64,
    fingerprint: [u8; 32],
    author: &'a str,
    body: &'a str,
    anchor: Option<&'a DiscussionAnchor>,
    outdated: bool,
}

struct Found {
    witness: Witness,
    kind: dto::NoteKind,
    author: String,
    path: Option<String>,
    line: Option<u32>,
    snippet: String,
    highlight_start: usize,
    highlight_end: usize,
}

#[derive(Default)]
struct Scan {
    items: Vec<Found>,
    next: Option<Point>,
    limited: bool,
}

struct Cancel(Arc<AtomicBool>);

impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub(in crate::entry) async fn search_notes(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<dto::DiscussionSearchResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse_review(&review)?;
    if !valid_revision(&query.revision) {
        return Err(AppError::InvalidRequest.into());
    }
    let literal = Literal::new(&query.query).ok_or(AppError::InvalidRequest)?;
    let query_hash = Sha256::digest(literal.folded().as_bytes()).into();
    let source = read_source(&entry, &actor, &review_id).await?;
    let epoch = entry
        .store
        .discussion_epoch(review_id.clone(), source.context.revision)
        .await?
        .cursor;
    let fresh = Scope::new(&review_id, &source, query.revision, epoch);
    let (mut scope, after, limited) = if let Some(cursor) = query.cursor {
        let saved: Position = entry.auth.endpoint.decode_cursor(CURSOR, &cursor)?;
        if saved.query != query_hash {
            return Err(AppError::CursorExpired.into());
        }
        (fresh.continued(saved.scope)?, saved.after, saved.limited)
    } else {
        (
            fresh,
            After::Published {
                page: 1,
                limit: DISCUSSION_LIMIT,
                point: Point::default(),
            },
            false,
        )
    };
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let stop = cancel.0.clone();
    let mut drafts_unavailable = false;
    let (page, next) = match after {
        After::Published { page, limit, point } => {
            let (read, current, physical_limit) =
                match read_search_page(&source, &scope, page, limit, point).await {
                    Err(error) if loses_access(&error) => {
                        entry
                            .store
                            .invalidate_discussions(
                                review_id,
                                source.context.revision,
                                epoch,
                                DiscussionInvalidation::Denied,
                            )
                            .await?;
                        return Err(error.into());
                    }
                    result => result?,
                };
            scope.observation = Some(read.observation);
            let next_page = read.next_page.map(|next| {
                discussion_page_at((next - 1) * u32::from(physical_limit), DISCUSSION_LIMIT)
            });
            let skipped = read.limited;
            let permit = SCAN_SLOTS.acquire().await.map_err(|_| AppError::Internal)?;
            let page = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                scan(
                    read.items.iter().flat_map(|thread| {
                        thread.notes.iter().map(|note| Candidate {
                            thread_id: Some(&thread.id),
                            note_id: note.id,
                            fingerprint: thread.fingerprint,
                            author: &note.author,
                            body: &note.body,
                            anchor: thread.anchor.as_ref(),
                            outdated: thread.outdated,
                        })
                    }),
                    point,
                    &literal,
                    &stop,
                )
            })
            .await
            .map_err(|_| AppError::Internal)?;
            let next = if let Some(point) = page.next {
                Some(After::Published {
                    page: current,
                    limit: physical_limit,
                    point,
                })
            } else if let Some((page, limit)) = next_page.filter(|(page, _)| *page <= 1000) {
                Some(After::Published {
                    page,
                    limit,
                    point: Point::default(),
                })
            } else if scope.account.is_some() {
                Some(After::Drafts {
                    point: Point::default(),
                })
            } else {
                None
            };
            let page = Scan {
                limited: skipped || page.limited || next_page.is_some_and(|(page, _)| page > 1000),
                ..page
            };
            (page, next)
        }
        After::Drafts { point } => {
            let provider = source
                .context
                .reader
                .as_ref()
                .ok_or(AppError::Unavailable)?;
            let read = provider
                .discussion_drafts(source.value.identity.clone(), scope.provider()?)
                .await;
            match read {
                Ok(read) => {
                    scope.observation = Some(read.observation);
                    let skipped = read.limited;
                    let permit = SCAN_SLOTS.acquire().await.map_err(|_| AppError::Internal)?;
                    let page = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        scan(
                            read.items.iter().map(|draft| Candidate {
                                thread_id: None,
                                note_id: draft.id,
                                fingerprint: draft.fingerprint,
                                author: &draft.author,
                                body: &draft.body,
                                anchor: draft.anchor.as_ref(),
                                outdated: draft.outdated,
                            }),
                            point,
                            &literal,
                            &stop,
                        )
                    })
                    .await
                    .map_err(|_| AppError::Internal)?;
                    let next = page.next.map(|point| After::Drafts { point });
                    (
                        Scan {
                            limited: skipped || page.limited,
                            ..page
                        },
                        next,
                    )
                }
                Err(AppError::CursorExpired) => {
                    entry
                        .store
                        .invalidate_discussions(
                            review_id,
                            source.context.revision,
                            epoch,
                            DiscussionInvalidation::Changed,
                        )
                        .await?;
                    return Err(AppError::CursorExpired.into());
                }
                Err(error) if error.source_unreadable() || error == AppError::PayloadTooLarge => {
                    drafts_unavailable = true;
                    (
                        Scan {
                            limited: true,
                            ..Scan::default()
                        },
                        None,
                    )
                }
                Err(error) => return Err(error.into()),
            }
        }
    };
    check_discussions(&entry, &actor, &review_id, &source, epoch).await?;
    let items = page
        .items
        .into_iter()
        .map(|found| {
            let hit = token(
                &entry,
                HIT,
                &Hit {
                    scope: scope.clone(),
                    witness: found.witness,
                },
            )?;
            Ok(dto::DiscussionSearchMatch {
                hit,
                kind: found.kind,
                author: found.author,
                path: found.path,
                line: found.line.map(|line| count(line as usize)).transpose()?,
                snippet: found.snippet,
                highlight_start: count(found.highlight_start)?,
                highlight_end: count(found.highlight_end)?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let limited = limited || page.limited;
    let coverage = match (next.is_some(), limited) {
        (true, _) => dto::PatchSearchCoverage::Reading,
        (false, true) => dto::PatchSearchCoverage::Limited,
        (false, false) => dto::PatchSearchCoverage::Complete,
    };
    let revision = patch_revision(&scope.revision)?;
    let continuation = next
        .map(|after| {
            token(
                &entry,
                CURSOR,
                &Position {
                    scope,
                    query: query_hash,
                    after,
                    limited,
                },
            )
        })
        .transpose()?;
    Ok(Json(Success {
        cursor: event_cursor(source.cursor)?,
        data: dto::DiscussionSearch {
            review_id: ReviewIdDto::new(review).ok_or(AppError::Internal)?,
            revision,
            coverage,
            drafts_unavailable,
            page: reviewq_protocol::Page::new(items, continuation).ok_or(AppError::Internal)?,
        },
    }))
}

async fn read_search_page(
    source: &super::SelectedSource,
    scope: &Scope,
    mut page: u32,
    mut limit: u16,
    point: Point,
) -> Result<(DiscussionPage, u32, u16), AppError> {
    let provider = source
        .context
        .reader
        .as_ref()
        .ok_or(AppError::Unavailable)?;
    loop {
        let request = DiscussionPageRequest {
            scope: scope.provider()?,
            page,
            limit,
            allow_partial: true,
        };
        match provider
            .discussions(source.value.identity.clone(), request)
            .await
        {
            Err(AppError::PayloadTooLarge) if point.note == 0 && point.byte == 0 && limit > 1 => {
                (page, limit) = discussion_page_at((page - 1) * u32::from(limit), limit - 1);
                if page > 1000 {
                    return Err(AppError::PayloadTooLarge);
                }
            }
            result => return Ok((result?, page, limit)),
        }
    }
}

fn scan<'a>(
    candidates: impl Iterator<Item = Candidate<'a>>,
    start: Point,
    query: &Literal,
    stop: &AtomicBool,
) -> Scan {
    let mut page = Scan::default();
    let mut bytes = 0;
    for (note, candidate) in candidates.enumerate().skip(start.note) {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        bytes += candidate.body.len();
        let from = if note == start.note { start.byte } else { 0 };
        if bytes > MAX_SCAN_BYTES {
            page.next = Some(Point { note, byte: from });
            break;
        }
        let Some(text) = displayed_markdown(candidate.body) else {
            page.limited = true;
            continue;
        };
        let folded = FoldedText::new(&text);
        for span in folded
            .occurrences(query)
            .filter(|span| span.start.byte >= from)
        {
            if page.items.len() == MAX_HITS {
                page.next = Some(Point {
                    note,
                    byte: span.start.byte,
                });
                return page;
            }
            page.items
                .push(candidate.found(note_occurrence(&text, span)));
        }
    }
    page
}

impl Candidate<'_> {
    fn found(&self, item: NoteOccurrence) -> Found {
        let address = match self.thread_id {
            Some(thread) => Address::Published {
                thread: thread.to_owned(),
                note: self.note_id,
            },
            None => Address::Draft { note: self.note_id },
        };
        let kind = self.thread_id.map_or(dto::NoteKind::Draft, |_| {
            note_kind(self.outdated, self.anchor.is_some())
        });
        let (path, line) = self.anchor.map_or((None, None), |anchor| {
            if let Some(line) = anchor.new_line {
                (Some(anchor.new_path.clone()), Some(line))
            } else {
                (Some(anchor.old_path.clone()), anchor.old_line)
            }
        });
        Found {
            witness: Witness {
                address,
                fingerprint: self.fingerprint,
                matched: item.matched,
                start: item.span.start.utf16,
                end: item.span.end.utf16,
                ordinal: item.ordinal,
            },
            kind,
            author: self.author.to_owned(),
            path,
            line,
            snippet: item.snippet,
            highlight_start: item.highlight_start,
            highlight_end: item.highlight_end,
        }
    }
}
