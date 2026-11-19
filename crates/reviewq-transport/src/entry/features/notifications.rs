use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::events::Snapshot;
use reviewq_app::notifications::{
    self as app, InboxPosition, InboxView, Kind, MAX_INBOX_PAGE, NotificationAction,
};
use reviewq_domain::identity::Revision;
use reviewq_protocol::notifications::{self as dto, Tab};
use reviewq_protocol::{
    MergeRequestIidDto, Page, PageCursorDto, ResourceIdDto, ReviewIdDto, RunIdDto, SessionKeyDto,
    Success, TicketKeyDto, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use crate::entry::error::{ApiBytes, ApiQuery as Query};
use crate::entry::pages::{count, parse, success};
use crate::entry::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InboxQuery {
    #[serde(default)]
    view: dto::InboxView,
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Deserialize, Serialize)]
struct InboxCursor {
    scope: [u8; 32],
    limit: u16,
    view: dto::InboxView,
    watermark: i64,
    revision: u64,
    occurred_at_ms: i64,
    source: String,
    id: String,
}

impl InboxCursor {
    fn app(self) -> Result<InboxPosition, AppError> {
        Ok(InboxPosition {
            scope: self.scope,
            limit: self.limit,
            view: app_view(self.view),
            watermark: self.watermark,
            revision: Revision::try_from(self.revision).map_err(|_| AppError::CursorExpired)?,
            occurred_at_ms: self.occurred_at_ms,
            source: parse(&self.source)?,
            id: parse(&self.id)?,
        })
    }
}

pub(super) async fn list(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Query(query): Query<InboxQuery>,
) -> Result<Json<Success<dto::NotificationPage>>, ApiError> {
    const SCOPE: &[u8] = b"browser-inbox";
    let actor = entry.auth.browser_actor(&headers, false)?;
    let limit = query.limit.unwrap_or(MAX_INBOX_PAGE);
    let endpoint = &entry.auth.endpoint;
    let position = query
        .cursor
        .as_deref()
        .map(|token| endpoint.decode_cursor::<InboxCursor>(SCOPE, token)?.app())
        .transpose()?;
    let stored = entry
        .store
        .read_inbox(actor, app_view(query.view), position, limit)
        .await?;
    let next = stored
        .value
        .next
        .map(|position| -> Result<_, AppError> {
            let token = endpoint.encode_cursor(
                SCOPE,
                &InboxCursor {
                    scope: position.scope,
                    limit: position.limit,
                    view: query.view,
                    watermark: position.watermark,
                    revision: position.revision.get(),
                    occurred_at_ms: position.occurred_at_ms,
                    source: position.source.to_string(),
                    id: position.id.to_string(),
                },
            )?;
            PageCursorDto::new(token).ok_or(AppError::Internal)
        })
        .transpose()?;
    let items = stored
        .value
        .items
        .into_iter()
        .map(notification)
        .collect::<Result<Vec<_>, _>>()?;
    let page = Page::new(items, next).ok_or(AppError::Internal)?;
    Ok(Json(success(
        dto::NotificationPage {
            page,
            counts: counts(stored.value.counts)?,
            review_contexts: stored
                .value
                .review_contexts
                .into_iter()
                .map(review_context)
                .collect::<Result<_, _>>()?,
        },
        stored.cursor,
    )?))
}

fn review_context(value: app::ReviewContext) -> Result<dto::ReviewContext, AppError> {
    Ok(dto::ReviewContext {
        review_id: ReviewIdDto::new(value.review_id.as_str()).ok_or(AppError::Internal)?,
        title: value.title,
        project_path: value.project_path,
        iid: MergeRequestIidDto::new(value.iid.get()).ok_or(AppError::Internal)?,
        ticket_keys: value
            .ticket_keys
            .iter()
            .map(|key| TicketKeyDto::new(key.as_str()).ok_or(AppError::Internal))
            .collect::<Result<_, _>>()?,
        our_comments: count(value.our_comments)?,
        unread_replies: count(value.unread_replies)?,
        personal_approval: value.personal_approval,
    })
}

pub(super) async fn update(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::StateResult>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::StateRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let action = match input.action {
        dto::Action::Read => NotificationAction::Read,
        dto::Action::Unread => NotificationAction::Unread,
        dto::Action::Dismiss => NotificationAction::Dismiss,
    };
    let ids = input
        .ids
        .iter()
        .map(|id| parse(id.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let stored = entry.store.assign_notifications(actor, action, ids).await?;
    Ok(Json(project_change(stored)?))
}

pub(in crate::entry) fn project_change(
    stored: Snapshot<app::InboxChange>,
) -> Result<Success<dto::StateResult>, AppError> {
    let affected_ids = stored
        .value
        .affected
        .iter()
        .map(|id| ResourceIdDto::new(id.as_str()).ok_or(AppError::Internal))
        .collect::<Result<Vec<_>, _>>()?;
    success(
        dto::StateResult {
            affected_ids,
            counts: counts(stored.value.counts)?,
        },
        stored.cursor,
    )
}

fn app_view(view: dto::InboxView) -> InboxView {
    match view {
        dto::InboxView::Unread => InboxView::Unread,
        dto::InboxView::All => InboxView::All,
    }
}

fn counts(value: app::UnreadCounts) -> Result<dto::UnreadCounts, AppError> {
    Ok(dto::UnreadCounts {
        unread_event_count: count(value.event_count)?,
        unread_mr_count: count(value.mr_count)?,
    })
}

fn notification(value: app::Notification) -> Result<dto::Notification, AppError> {
    let time = |time| UtcTimestampDto::from_unix_milliseconds(time).ok_or(AppError::Internal);
    let tab = match value.kind {
        Kind::Reply | Kind::EditedReply | Kind::Resolved | Kind::Reopened | Kind::NewHead => {
            Tab::Changes
        }
        Kind::AgentQuestion | Kind::AgentPermission | Kind::AgentFailure | Kind::Quota => {
            Tab::Agent
        }
        Kind::ReportReady | Kind::ReportFailed | Kind::FollowupReady => Tab::Findings,
        _ => Tab::Overview,
    };
    Ok(dto::Notification {
        id: ResourceIdDto::new(value.id.as_str()).ok_or(AppError::Internal)?,
        source: value.source.to_string(),
        kind: value.kind.to_string(),
        review_id: ReviewIdDto::new(value.review.as_str()).ok_or(AppError::Internal)?,
        run_id: value
            .run
            .map(|id| RunIdDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        session_key: value
            .session
            .map(|id| SessionKeyDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        label: value.label,
        event_ref: value
            .event_ref
            .map(|id| ResourceIdDto::new(id).ok_or(AppError::Internal))
            .transpose()?,
        occurred_at: time(value.occurred_at_ms)?,
        read_at: value.read_at_ms.map(time).transpose()?,
        notified_at: value.notified_at_ms.map(time).transpose()?,
        desktop_status: value.desktop_status,
        desktop_reason: value.desktop_reason,
        tab,
    })
}
