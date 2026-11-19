use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Comments {
    limit: Option<u16>,
    cursor: Option<String>,
    #[serde(default)]
    order: CommentOrder,
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
) -> Result<Json<dto::TicketDetailResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let key: TicketKey = parse(&raw_key.to_ascii_uppercase())?;
    let limit = query.limit.unwrap_or(20);
    if !(1..=25).contains(&limit) {
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
    if position.as_ref().is_some_and(|item| {
        item.settings != active.revision.get()
            || item.key != key.as_str()
            || item.limit != limit
            || item.order != query.order
            || item.offset == 0
    }) {
        return Err(AppError::CursorExpired.into());
    }
    let provider = active.tickets.ok_or(AppError::Unavailable)?;
    let (offset, page_limit) = position
        .as_ref()
        .map_or((0, limit), |item| (item.offset - 1, limit + 1));
    let mut result = provider
        .issue_detail(
            key.clone(),
            allowed.value.estimate_fields.clone(),
            offset,
            page_limit,
            match query.order {
                CommentOrder::Newest => reviewq_app::sources::CommentOrder::Newest,
                CommentOrder::Oldest => reviewq_app::sources::CommentOrder::Oldest,
            },
            position.is_none(),
        )
        .await?;
    if let Some(position) = position {
        let witness = result
            .comments
            .first()
            .map(|comment| (comment.created_at_ms, comment.id));
        if position.issue != result.id || witness != Some(position.previous) {
            provider.invalidate_issue_details(Some(&key));
            return Err(AppError::CursorExpired.into());
        }
        result.comments.remove(0);
    }
    let next = result
        .next_offset
        .map(|offset| {
            let previous = result.comments.last().ok_or(AppError::CursorExpired)?;
            let cursor = endpoint.encode_cursor(
                COMMENTS,
                &CommentPosition {
                    settings: active.revision.get(),
                    limit,
                    key: key.to_string(),
                    issue: result.id,
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
    let comments = result
        .comments
        .into_iter()
        .map(|comment| {
            Ok(dto::TicketComment {
                id: UpstreamIdDto::new(comment.id).ok_or(AppError::Unavailable)?,
                url: format!(
                    "{}?focusedCommentId={}#comment-{}",
                    result.url, comment.id, comment.id
                ),
                author: comment.author,
                author_id: comment.author_id,
                body: comment.body,
                created_at: timestamp(comment.created_at_ms)?,
                updated_at: comment.updated_at_ms.map(timestamp).transpose()?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(success(
        dto::TicketDetail {
            key: TicketKeyDto::new(key.as_str()).ok_or(AppError::Internal)?,
            title: result.identity.title,
            url: result.url,
            status: result.status,
            priority: result.priority,
            assignee: result.assignee,
            assignee_id: result.assignee_id,
            reporter: result.reporter,
            reporter_id: result.reporter_id,
            issue_type: result.issue_type,
            labels: result.labels,
            components: result.components,
            description: result.description,
            estimate: result.estimate,
            updated_at: result.updated_at_ms.map(timestamp).transpose()?,
            fetched_at: timestamp(result.fetched_at_ms)?,
            warnings: result.warning.into_iter().collect(),
            comment_count: super::pages::count(u64::from(result.comment_count))?,
            comments: Page::new(comments, next).ok_or(AppError::Internal)?,
        },
        allowed.cursor,
    )?))
}

fn timestamp(value: i64) -> Result<UtcTimestampDto, AppError> {
    UtcTimestampDto::from_unix_milliseconds(value).ok_or(AppError::Unavailable)
}
