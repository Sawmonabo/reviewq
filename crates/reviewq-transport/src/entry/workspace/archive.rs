use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{OriginalUri, State};
use axum::http::HeaderMap;
use reviewq_app::access::{Actor, AppError};
use reviewq_app::queue::{
    ArchiveDate, ArchiveFilter, ArchiveMetadata, ArchivePosition, ArchiveScope, MAX_QUEUE_PAGE,
    read_archive_metadata,
};
use reviewq_protocol::queue as dto;
use reviewq_protocol::{Page, PageCursorDto, RunIdDto, UpstreamIdDto, UtcTimestampDto};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::entry::pages::{Context, count, parse, project_review, success};
use crate::entry::{ApiError, Entry};

use super::{queue, signature, sprints};

const CURSOR: &[u8] = b"reviewq-archived-reviews-v1";

#[derive(Default)]
pub(in crate::entry) struct ArchiveCache {
    current: Mutex<Option<CachedMetadata>>,
}

struct CachedMetadata {
    coverage: Arc<ArchiveScope>,
    membership: Arc<ArchiveMetadata>,
    generation: [u8; 16],
    expires: Instant,
}

#[derive(Deserialize, Serialize)]
struct Cursor {
    context: Context,
    fingerprint: [u8; 32],
    metadata: [u8; 16],
    latest: Option<i64>,
    review: String,
}

pub(in crate::entry) async fn list(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Result<Json<dto::ArchivePageResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let (query, limit, cursor) = parse_query(uri.query().unwrap_or_default())?;
    let fingerprint = signature(&(
        &query.search,
        query.from,
        query.until,
        query
            .pairs
            .iter()
            .map(|pair| (pair.board, pair.sprint))
            .collect::<Vec<_>>(),
    ))?;
    let (scope, metadata, generation) = metadata(&entry, &actor).await?;
    let generation = if query.pairs.is_empty() {
        [0; 16]
    } else {
        generation
    };
    let endpoint = &entry.auth.endpoint;
    let after = cursor
        .as_deref()
        .map(|token| {
            let cursor: Cursor = endpoint.decode_cursor(CURSOR, token)?;
            if cursor.fingerprint != fingerprint || cursor.metadata != generation {
                return Err(AppError::CursorExpired);
            }
            Ok(ArchivePosition {
                context: cursor.context.try_into()?,
                latest: cursor.latest,
                review: parse(&cursor.review)?,
            })
        })
        .transpose()?;
    let limited = scope.limited;
    let page = entry
        .store
        .read_archive(actor, scope, query, metadata.clone(), after, limit)
        .await?;
    let next = page
        .value
        .next
        .map(|position| {
            let token = endpoint.encode_cursor(
                CURSOR,
                &Cursor {
                    context: position.context.into(),
                    fingerprint,
                    metadata: generation,
                    latest: position.latest,
                    review: position.review.to_string(),
                },
            )?;
            PageCursorDto::new(token).ok_or(AppError::Internal)
        })
        .transpose()?;
    let items = page
        .value
        .items
        .into_iter()
        .map(|item| {
            let membership = if item.membership.complete {
                dto::ArchiveMembership::Verified
            } else if !item.membership.pairs.is_empty() {
                dto::ArchiveMembership::Partial
            } else {
                dto::ArchiveMembership::Unknown
            };
            Ok(dto::ArchiveItem {
                card: dto::RunCard {
                    review: project_review(&item.card.review)?,
                    run_ids: item
                        .card
                        .runs
                        .into_iter()
                        .map(|id| RunIdDto::new(id.as_str()).ok_or(AppError::Internal))
                        .collect::<Result<_, _>>()?,
                },
                latest_matching_review_at: item
                    .latest
                    .and_then(UtcTimestampDto::from_unix_milliseconds),
                date_source: item.date.map(|source| match source {
                    ArchiveDate::Decision => dto::ArchiveDate::Decision,
                    ArchiveDate::Report => dto::ArchiveDate::Report,
                    ArchiveDate::Followup => dto::ArchiveDate::Followup,
                }),
                sprint_pairs: item
                    .membership
                    .pairs
                    .into_iter()
                    .map(sprints::project)
                    .collect::<Result<_, _>>()?,
                membership,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let boards = metadata
        .boards
        .iter()
        .map(|board| {
            Ok(dto::ArchiveBoard {
                id: UpstreamIdDto::new(board.id).ok_or(AppError::Internal)?,
                label: board.label.clone(),
                available: board.available && !limited,
            })
        })
        .collect::<Result<_, AppError>>()?;
    Ok(Json(success(
        dto::ArchivePage {
            page: Page::new(items, next).ok_or(AppError::Internal)?,
            total_archived: count(page.value.total)?,
            total_matching: count(page.value.matching)?,
            boards,
        },
        page.cursor,
    )?))
}

async fn metadata(
    entry: &Entry,
    actor: &Actor,
) -> Result<(Arc<ArchiveScope>, Arc<ArchiveMetadata>, [u8; 16]), AppError> {
    let mut cache = tokio::time::timeout(
        Duration::from_secs(30),
        entry.archive_metadata.current.lock(),
    )
    .await
    .map_err(|_| AppError::Busy)?;
    let scope = entry.store.archive_scope(actor.clone()).await?.value;
    let active = entry.active_source().await?;
    if scope.settings != active.revision {
        return Err(AppError::StaleRevision);
    }
    if let Some(cached) = cache
        .as_ref()
        .filter(|cached| cached.coverage.as_ref() == &scope && cached.expires > Instant::now())
    {
        return Ok((
            cached.coverage.clone(),
            cached.membership.clone(),
            cached.generation,
        ));
    }
    let membership =
        read_archive_metadata(&active.scope, active.tickets.as_deref(), &scope.targets).await?;
    let ttl = if membership.boards.iter().all(|board| board.available) {
        300
    } else {
        30
    };
    let generation = if let Some(previous) = cache.as_ref().filter(|previous| {
        previous.coverage.as_ref() == &scope
            && previous.membership.membership == membership.membership
    }) {
        previous.generation
    } else {
        let mut next = [0; 16];
        getrandom::fill(&mut next).map_err(|_| AppError::Internal)?;
        next
    };
    let scope = Arc::new(scope);
    let membership = Arc::new(membership);
    *cache = Some(CachedMetadata {
        coverage: scope.clone(),
        membership: membership.clone(),
        generation,
        expires: Instant::now() + Duration::from_secs(ttl),
    });
    Ok((scope, membership, generation))
}

fn parse_query(raw: &str) -> Result<(ArchiveFilter, u16, Option<String>), AppError> {
    if raw.len() > 8192 {
        return Err(AppError::InvalidRequest);
    }
    let mut query = ArchiveFilter {
        search: String::new(),
        from: None,
        until: None,
        pairs: Vec::new(),
    };
    let mut limit = 50;
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    for (name, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if name != "sprintPair" && !seen.insert(name.to_string()) {
            return Err(AppError::InvalidRequest);
        }
        match name.as_ref() {
            "q" if value.chars().count() <= 200 => query.search = value.to_lowercase(),
            "fromInclusive" => {
                query.from = Some(dto::parse_archive_bound(&value).ok_or(AppError::InvalidRequest)?)
            }
            "untilExclusive" => {
                query.until =
                    Some(dto::parse_archive_bound(&value).ok_or(AppError::InvalidRequest)?)
            }
            "sprintPair" if query.pairs.len() < 32 => query.pairs.push(queue::pair(&value)?),
            "limit" => limit = value.parse().map_err(|_| AppError::InvalidRequest)?,
            "cursor" => cursor = Some(value.to_string()),
            _ => return Err(AppError::InvalidRequest),
        }
    }
    if !(1..=MAX_QUEUE_PAGE).contains(&limit)
        || matches!((query.from,query.until),(Some(a),Some(b)) if a>=b)
    {
        return Err(AppError::InvalidRequest);
    }
    query.pairs.sort_unstable();
    query.pairs.dedup();
    Ok((query, limit, cursor))
}
