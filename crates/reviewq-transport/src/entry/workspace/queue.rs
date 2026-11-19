use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::{OriginalUri, State};
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::queue::{
    MAX_QUEUE_PAGE, QueueFacet, QueueFilter, QueueListItem, QueueOrder, QueueOwnership, QueuePhase,
    QueuePosition, QueueSortKey, SprintFilter, SprintPair,
};
use reviewq_domain::settings::{MAX_CONFIGURED_BOARDS, MAX_CONFIGURED_REPOSITORIES};
use reviewq_protocol::queue as dto;
use reviewq_protocol::{Page, PageCursorDto, RevisionDto, TicketKeyDto, UpstreamIdDto};
use serde::{Deserialize, Serialize};

use crate::entry::pages::{Context, count, project_review, queue_sync, success};
use crate::entry::{ApiError, Entry};

use super::signature;

const CURSOR: &[u8] = b"reviewq-queue-filter-v1";

#[derive(Deserialize, Serialize)]
struct Position {
    context: Context,
    filter: [u8; 32],
    key: (i64, i64, String, i64, String, String),
}

pub(in crate::entry) async fn list(
    State(entry): State<Arc<Entry>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Result<Json<dto::QueuePageResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let (query, limit, cursor) = parse_query(uri.query().unwrap_or_default())?;
    let filter = signature(&(
        query.empty_scope,
        &query.boards,
        &query.projects,
        query.sprint as u8,
        query
            .pairs
            .iter()
            .map(|pair| (pair.board, pair.sprint))
            .collect::<Vec<_>>(),
        query.ownership as u8,
        query.author,
        query.phase as u8,
        query.hide_reviewed,
        query.hide_in_progress,
        &query.search,
        query.order as u8,
    ))?;
    let endpoint = &entry.auth.endpoint;
    let after = cursor
        .as_deref()
        .map(|cursor| {
            let pos: Position = endpoint.decode_cursor(CURSOR, cursor)?;
            if pos.filter != filter {
                return Err(AppError::CursorExpired);
            }
            let (group, time, label, digits, number, id) = pos.key;
            Ok(QueuePosition {
                context: pos.context.try_into()?,
                key: QueueSortKey {
                    group,
                    time,
                    label,
                    digits,
                    number,
                    id,
                },
            })
        })
        .transpose()?;
    let page = entry.store.read_queue(actor, query, after, limit).await?;
    let active = entry.active_source().await?;
    if active.revision != page.value.settings_revision {
        return Err(AppError::StaleRevision.into());
    }
    let next = page
        .value
        .next
        .map(|pos| {
            let key = pos.key;
            let token = endpoint.encode_cursor(
                CURSOR,
                &Position {
                    context: pos.context.into(),
                    filter,
                    key: (
                        key.group, key.time, key.label, key.digits, key.number, key.id,
                    ),
                },
            )?;
            PageCursorDto::new(token).ok_or(AppError::Internal)
        })
        .transpose()?;
    let items = page
        .value
        .items
        .into_iter()
        .map(|item| match item {
            QueueListItem::Review(item) => {
                project_review(&item).map(|row| dto::QueueRow::Mr(Box::new(row)))
            }
            QueueListItem::Unmatched(item) => {
                Ok(dto::QueueRow::UnmatchedTicket(dto::UnmatchedTicket {
                    key: TicketKeyDto::new(item.ticket.key.as_str()).ok_or(AppError::Internal)?,
                    title: item.ticket.title,
                    status: item.status,
                    priority: item.priority,
                    assignee: item.assignee,
                }))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let facets = page.value.facets;
    let sprints = facets
        .sprints
        .into_iter()
        .map(|(pair, label)| {
            Ok(dto::SprintFacet {
                board_id: upstream(pair.board)?,
                id: upstream(pair.sprint)?,
                label,
            })
        })
        .collect::<Result<_, AppError>>()?;
    Ok(Json(success(
        dto::QueuePage {
            page: Page::new(items, next).ok_or(AppError::Internal)?,
            total_rows: count(page.value.total)?,
            total_mrs: count(page.value.total_mrs)?,
            facets: dto::QueueFacets {
                boards: labels(facets.boards)?,
                projects: labels(facets.projects)?,
                authors: labels(facets.authors)?,
                sprints,
                hide_reviewed: count(facets.hide_reviewed)?,
                hide_in_progress: count(facets.hide_in_progress)?,
            },
            sync: queue_sync(&active, page.value.sync),
            settings_revision: RevisionDto::new(page.value.settings_revision.get())
                .ok_or(AppError::Internal)?,
        },
        page.cursor,
    )?))
}

fn labels(values: Vec<QueueFacet>) -> Result<Vec<dto::NamedUpstreamId>, AppError> {
    values
        .into_iter()
        .map(|item| {
            Ok(dto::NamedUpstreamId {
                id: upstream(item.id)?,
                label: item.label,
            })
        })
        .collect()
}

fn upstream(value: u64) -> Result<UpstreamIdDto, AppError> {
    UpstreamIdDto::new(value).ok_or(AppError::Internal)
}

fn parse_query(raw: &str) -> Result<(QueueFilter, u16, Option<String>), AppError> {
    // Permit every configured ID, the encoded search and a full-length page cursor together.
    if raw.len() > 24 * 1024 {
        return Err(AppError::InvalidRequest);
    }
    let mut filter = QueueFilter::default();
    let mut limit = 50;
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    for (field, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        let value = value.as_ref();
        if !matches!(field.as_ref(), "boardId" | "projectId" | "sprintPair")
            && !seen.insert(field.to_string())
        {
            return Err(AppError::InvalidRequest);
        }
        match field.as_ref() {
            "boardId" if filter.boards.len() < MAX_CONFIGURED_BOARDS => {
                filter.boards.push(number(value)?)
            }
            "projectId" if filter.projects.len() < MAX_CONFIGURED_REPOSITORIES => {
                filter.projects.push(number(value)?)
            }
            "sprintPair" if filter.pairs.len() < 32 => filter.pairs.push(pair(value)?),
            "scope" if value == "none" => filter.empty_scope = true,
            "sprint" => {
                filter.sprint = match value {
                    "any" => SprintFilter::Any,
                    "active" => SprintFilter::Active,
                    "none" => SprintFilter::None,
                    "selected" => SprintFilter::Selected,
                    _ => return Err(AppError::InvalidRequest),
                }
            }
            "ownership" => {
                filter.ownership = match value {
                    "all" => QueueOwnership::All,
                    "others" => QueueOwnership::Others,
                    "mine" => QueueOwnership::Mine,
                    _ => return Err(AppError::InvalidRequest),
                }
            }
            "state" => {
                filter.phase = match value {
                    "all_open" => QueuePhase::AllOpen,
                    "needs_review" => QueuePhase::NeedsReview,
                    "followup" => QueuePhase::Followup,
                    "waiting_author" => QueuePhase::WaitingAuthor,
                    "reviewed" => QueuePhase::Reviewed,
                    "archive" => QueuePhase::Archive,
                    _ => return Err(AppError::InvalidRequest),
                }
            }
            "sort" => {
                filter.order = match value {
                    "recent" => QueueOrder::Recent,
                    "oldest" => QueueOrder::Oldest,
                    "author" => QueueOrder::Author,
                    "ticket" => QueueOrder::Ticket,
                    _ => return Err(AppError::InvalidRequest),
                }
            }
            "authorId" => filter.author = Some(number(value)?),
            "hideReviewed" => {
                filter.hide_reviewed = value.parse().map_err(|_| AppError::InvalidRequest)?
            }
            "hideInProgress" => {
                filter.hide_in_progress = value.parse().map_err(|_| AppError::InvalidRequest)?
            }
            "q" if value.chars().count() <= 200 => filter.search = value.to_lowercase(),
            "limit" => limit = value.parse().map_err(|_| AppError::InvalidRequest)?,
            "cursor" => cursor = Some(value.to_owned()),
            _ => return Err(AppError::InvalidRequest),
        }
    }
    if !filter.pairs.is_empty() && !seen.contains("sprint") {
        filter.sprint = SprintFilter::Selected;
    }
    if !(1..=MAX_QUEUE_PAGE).contains(&limit)
        || (filter.sprint == SprintFilter::Selected) != !filter.pairs.is_empty()
    {
        return Err(AppError::InvalidRequest);
    }
    filter.boards.sort_unstable();
    filter.boards.dedup();
    filter.projects.sort_unstable();
    filter.projects.dedup();
    filter.pairs.sort_unstable();
    filter.pairs.dedup();
    Ok((filter, limit, cursor))
}

pub(super) fn pair(value: &str) -> Result<SprintPair, AppError> {
    let (board, sprint) = value.split_once(':').ok_or(AppError::InvalidRequest)?;
    Ok(SprintPair {
        board: number(board)?,
        sprint: number(sprint)?,
    })
}

fn number(value: &str) -> Result<u64, AppError> {
    let number = value.parse::<u64>().map_err(|_| AppError::InvalidRequest)?;
    if !(1..=i64::MAX as u64).contains(&number) || value != number.to_string() {
        return Err(AppError::InvalidRequest);
    }
    Ok(number)
}
