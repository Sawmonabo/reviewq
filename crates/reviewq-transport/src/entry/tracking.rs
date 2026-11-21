use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use reviewq_app::access::AppError;
use reviewq_app::queue::TrackReview;
use reviewq_domain::identity::{Revision, TicketKey};
use reviewq_protocol::queue as dto;
use reviewq_protocol::{
    GitShaDto, MergeRequestIidDto, Page, PageCursorDto, ProjectIdDto, ReviewIdDto, TicketKeyDto,
    UpstreamIdDto, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::error::{ApiBytes, ApiPath as Path, ApiQuery as Query};
use super::pages::{parse, success};
use super::{ApiError, Entry};

const COMMENTS: &[u8] = b"reviewq-jira-comments-v1";
const TICKET_SCOPE: &[u8] = b"reviewq-jira-ticket-scope-v1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Comments {
    limit: Option<u16>,
    cursor: Option<String>,
    scope: Option<String>,
    #[serde(default)]
    order: CommentOrder,
    #[serde(default)]
    view: TicketView,
}

#[derive(Clone, Copy, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TicketView {
    #[default]
    Detail,
    Comments,
}

#[derive(Clone, Copy, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CommentOrder {
    #[default]
    Newest,
    Oldest,
}

#[derive(Deserialize, Serialize)]
struct CommentPosition {
    settings: u64,
    limit: u16,
    key: String,
    issue: u64,
    offset: u32,
    previous: (i64, u64),
    order: CommentOrder,
}

#[derive(Deserialize, Serialize)]
struct TicketScope {
    settings: u64,
    key: String,
    issue: u64,
}

pub(super) async fn track(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Json<dto::TrackReviewResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let request: dto::TrackReviewRequest =
        serde_json::from_slice(&body).map_err(|_| AppError::InvalidRequest)?;
    let expected = Revision::try_from(request.expected_revision.get())
        .map_err(|_| AppError::InvalidRequest)?;
    let active = entry.active_source().await?;
    let scope = entry.store.workspace_context(actor.clone()).await?;
    if expected != active.revision || expected != scope.value.revision {
        return Err(AppError::StaleRevision.into());
    }
    let source = active.reader.ok_or(AppError::Unavailable)?;
    let prospective = source
        .trackable_review(scope.value.projects, request.url)
        .await?;
    let mut tickets = Vec::new();
    if !prospective.candidate_tickets.is_empty() {
        let jira = active.tickets.ok_or(AppError::TicketUnverified)?;
        for key in prospective.candidate_tickets {
            tickets.push(
                jira.issue_identity(key)
                    .await?
                    .ok_or(AppError::TicketUnverified)?,
            );
        }
    }
    let committed = entry
        .store
        .track(
            actor,
            TrackReview {
                settings: expected,
                before: scope.cursor,
                review: prospective.review,
                tickets,
            },
        )
        .await?;
    let id = committed.value.review.identity;
    Ok(Json(success(
        dto::TrackedReview {
            review_id: ReviewIdDto::new(committed.value.id.as_str()).ok_or(AppError::Internal)?,
            project_id: ProjectIdDto::new(id.project_id().get()).ok_or(AppError::Internal)?,
            iid: MergeRequestIidDto::new(id.iid().get()).ok_or(AppError::Internal)?,
            source_sha: GitShaDto::new(committed.value.review.head_sha.as_str())
                .ok_or(AppError::Internal)?,
        },
        committed.cursor,
    )?))
}

pub(super) async fn ticket(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(raw_key): Path<String>,
    Query(query): Query<Comments>,
) -> Result<Response, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let key: TicketKey = parse(&raw_key.to_ascii_uppercase())?;
    let limit = query.limit.unwrap_or(20);
    if !(1..=25).contains(&limit)
        || (query.view == TicketView::Comments) != query.scope.is_some()
        || (query.view == TicketView::Detail && query.cursor.is_some())
    {
        return Err(AppError::InvalidRequest.into());
    }
    let active = entry.active_source().await?;
    let allowed = entry
        .store
        .ticket_authority(actor.clone(), key.clone())
        .await?;
    if active.revision != allowed.value.settings {
        return Err(AppError::StaleRevision.into());
    }
    let endpoint = &entry.auth.endpoint;
    let position = query
        .cursor
        .as_deref()
        .map(|token| endpoint.decode_cursor::<CommentPosition>(COMMENTS, token))
        .transpose()?;
    let scope = query
        .scope
        .as_deref()
        .map(|token| endpoint.decode_cursor::<TicketScope>(TICKET_SCOPE, token))
        .transpose()?;
    let settings = active.revision.get();
    if scope
        .as_ref()
        .is_some_and(|scope| scope.settings != settings || scope.key != key.as_str())
    {
        return Err(AppError::CursorExpired.into());
    }
    if position.as_ref().is_some_and(|position| {
        position.settings != settings
            || position.key != key.as_str()
            || position.limit != limit
            || position.order != query.order
            || position.offset == 0
            || scope
                .as_ref()
                .is_some_and(|scope| scope.issue != position.issue)
    }) {
        return Err(AppError::CursorExpired.into());
    }
    let provider = active.tickets.ok_or(AppError::Unavailable)?;
    let (offset, page_limit) = position
        .as_ref()
        .map_or((0, limit), |item| (item.offset - 1, limit + 1));
    let order = match query.order {
        CommentOrder::Newest => reviewq_app::sources::CommentOrder::Newest,
        CommentOrder::Oldest => reviewq_app::sources::CommentOrder::Oldest,
    };
    let (detail, mut page) = if let Some(scope) = scope {
        let page = provider
            .issue_comments(key.clone(), scope.issue, offset, page_limit, order)
            .await?;
        (None, page)
    } else {
        let opened = provider
            .issue_detail(
                key.clone(),
                allowed.value.estimate_fields.clone(),
                limit,
                order,
            )
            .await?;
        (Some(opened.detail), opened.comments)
    };
    // Read one overlapping comment from Jira to detect a shifted or exhausted boundary.
    if let Some(position) = position {
        let witness = page
            .items
            .first()
            .map(|comment| (comment.created_at_ms, comment.id));
        if position.issue != page.issue
            || witness != Some(position.previous)
            || page.items.len() == 1
        {
            provider.invalidate_issue_details(Some(&key));
            return Err(AppError::CursorExpired.into());
        }
        page.items.remove(0);
    }
    let next = page
        .next_offset
        .map(|offset| {
            let previous = page.items.last().ok_or(AppError::CursorExpired)?;
            let cursor = endpoint.encode_cursor(
                COMMENTS,
                &CommentPosition {
                    settings,
                    limit,
                    key: key.to_string(),
                    issue: page.issue,
                    offset,
                    previous: (previous.created_at_ms, previous.id),
                    order: query.order,
                },
            )?;
            PageCursorDto::new(cursor).ok_or(AppError::Internal)
        })
        .transpose()?;
    let current = entry.store.ticket_authority(actor, key.clone()).await?;
    if current.value != allowed.value {
        return Err(AppError::StaleRevision.into());
    }
    let comments = page
        .items
        .into_iter()
        .map(|comment| {
            Ok(dto::TicketComment {
                id: UpstreamIdDto::new(comment.id).ok_or(AppError::Unavailable)?,
                url: format!(
                    "{}?focusedCommentId={}#comment-{}",
                    page.url, comment.id, comment.id
                ),
                author: comment.author,
                author_id: comment.author_id,
                body: comment.body,
                created_at: timestamp(comment.created_at_ms)?,
                updated_at: comment.updated_at_ms.map(timestamp).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    // The service signs an issue scope independently of any particular sort or page boundary.
    let comment_scope = endpoint.encode_cursor(
        TICKET_SCOPE,
        &TicketScope {
            settings,
            key: key.to_string(),
            issue: page.issue,
        },
    )?;
    let comment_scope = PageCursorDto::new(comment_scope).ok_or(AppError::Internal)?;
    let key = TicketKeyDto::new(key.as_str()).ok_or(AppError::Internal)?;
    let comments = Page::new(comments, next).ok_or(AppError::Internal)?;
    let fetched_at = timestamp(page.fetched_at_ms)?;
    let comment_count = super::pages::count(u64::from(page.total))?;
    if let Some(detail) = detail {
        let body = dto::TicketDetail {
            key,
            title: detail.identity.title,
            url: page.url,
            status: detail.status,
            priority: detail.priority,
            assignee: detail.assignee,
            assignee_id: detail.assignee_id,
            reporter: detail.reporter,
            reporter_id: detail.reporter_id,
            issue_type: detail.issue_type,
            labels: detail.labels,
            components: detail.components,
            description: detail.description,
            estimate: detail.estimate,
            updated_at: detail.updated_at_ms.map(timestamp).transpose()?,
            fetched_at,
            warnings: detail.warning.into_iter().collect(),
            comment_scope,
            comment_count,
            comments,
        };
        Ok(Json(success(body, allowed.cursor)?).into_response())
    } else {
        let body = dto::TicketComments {
            key,
            comment_scope,
            comment_count,
            fetched_at,
            comments,
        };
        Ok(Json(success(body, allowed.cursor)?).into_response())
    }
}

fn timestamp(value: i64) -> Result<UtcTimestampDto, AppError> {
    UtcTimestampDto::from_unix_milliseconds(value).ok_or(AppError::Unavailable)
}
