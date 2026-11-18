use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::Mac;
use reviewq_app::access::AppError;
use reviewq_app::events::Snapshot;
use reviewq_app::queue::{
    MAX_QUEUE_PAGE, MergeRequestState, PageContext, QueueItem, QueuePosition, RunPosition,
    SessionPosition,
};
use reviewq_domain::identity::{EventCursor, ReviewId, RunId};
use reviewq_domain::session::HostProvider;
use reviewq_protocol::queue::{
    MergeRequestRow, MergeRequestStatus, QueueFacets, QueuePage, QueuePageResponse, QueueRow,
    QueueStatus, QueueSync, QueueSyncMode, ReviewOverview, ReviewOverviewResponse, ReviewSelection,
    ReviewSelectionResponse, RunSummary,
};
use reviewq_protocol::{
    EventCursorDto, GitShaDto, HostProviderDto, MergeRequestIidDto, Page, PageCursorDto,
    ProjectIdDto, ReviewIdDto, RevisionDto, RunDetail, RunDetailResponse, RunIdDto, SafeCountDto,
    SessionKeyDto, SessionSummary, Success, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::{ApiError, Entry, ServiceEndpoint};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageQuery {
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct SelectionQuery {
    review_id: String,
    run_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PagePosition {
    Queue {
        context: Context,
        updated_at_ms: i64,
        review_id: String,
    },
    Review {
        context: Context,
        review_id: String,
        started_at_ms: i64,
        run_id: String,
    },
    Run {
        context: Context,
        run_id: String,
        session_key: String,
    },
}

#[derive(Serialize, Deserialize)]
struct Context {
    generation: u64,
    authority: [u8; 32],
    limit: u16,
}

pub(super) async fn queue(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<QueuePageResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let limit = query.limit()?;
    let endpoint = &entry.auth.endpoint;
    let after = match query.position(endpoint)? {
        None => None,
        Some(PagePosition::Queue {
            context,
            updated_at_ms,
            review_id,
        }) => Some(QueuePosition {
            context: context.try_into()?,
            updated_at_ms,
            review_id: parse(&review_id)?,
        }),
        _ => return Err(AppError::CursorExpired.into()),
    };
    let Snapshot { value, cursor } = entry.store.read_queue(actor, after, limit).await?;
    let count = count(value.total)?;
    let rows = value
        .items
        .iter()
        .map(|item| project_review(item).map(|row| QueueRow::Mr(Box::new(row))))
        .collect::<Result<Vec<_>, _>>()?;
    let next = value
        .next
        .map(|pos| {
            endpoint.encode_page(PagePosition::Queue {
                context: pos.context.into(),
                updated_at_ms: pos.updated_at_ms,
                review_id: pos.review_id.to_string(),
            })
        })
        .transpose()?;
    Ok(Json(success(
        QueuePage {
            page: Page::new(rows, next).ok_or(AppError::Internal)?,
            total_rows: count,
            total_mrs: count,
            facets: QueueFacets {
                boards: Vec::new(),
                projects: Vec::new(),
                sprints: Vec::new(),
                authors: Vec::new(),
                hide_reviewed: self::count(0)?,
                hide_in_progress: self::count(0)?,
            },
            sync: QueueSync {
                mode: if entry.source.is_some() {
                    QueueSyncMode::Connected
                } else {
                    QueueSyncMode::Setup
                },
                active: false,
                stale: false,
                last_attempt_at: None,
                last_success_at: None,
                error: None,
            },
            settings_revision: RevisionDto::new(value.settings_revision.get())
                .ok_or(AppError::Internal)?,
        },
        cursor,
    )?))
}

pub(super) async fn overview(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<ReviewOverviewResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let id: ReviewId = parse(&review_id)?;
    let limit = query.limit()?;
    let endpoint = &entry.auth.endpoint;
    let after = match query.position(endpoint)? {
        None => None,
        Some(PagePosition::Review {
            context,
            review_id,
            started_at_ms,
            run_id,
        }) => Some(RunPosition {
            context: context.try_into()?,
            review_id: parse(&review_id)?,
            started_at_ms,
            run_id: parse(&run_id)?,
        }),
        _ => return Err(AppError::CursorExpired.into()),
    };
    let Snapshot { value, cursor } = entry.store.read_review(actor, id, after, limit).await?;
    let runs = value
        .runs
        .iter()
        .map(|row| {
            Ok(RunSummary {
                run_id: RunIdDto::new(row.run_id.as_str()).ok_or(AppError::Internal)?,
                lead_session_key: SessionKeyDto::new(row.lead_session_key.as_str())
                    .ok_or(AppError::Internal)?,
                provider: provider(row.provider),
                started_at: UtcTimestampDto::from_unix_milliseconds(row.started_at_ms),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let next = value
        .next_run
        .map(|pos| {
            endpoint.encode_page(PagePosition::Review {
                context: pos.context.into(),
                review_id: pos.review_id.to_string(),
                started_at_ms: pos.started_at_ms,
                run_id: pos.run_id.to_string(),
            })
        })
        .transpose()?;
    let data = ReviewOverview {
        review: project_review(&value.item)?,
        decision_revision: RevisionDto::new(0).ok_or(AppError::Internal)?,
        settings_revision: RevisionDto::new(value.settings_revision.get())
            .ok_or(AppError::Internal)?,
        checkpoint_id: None,
        runs: Page::new(runs, next).ok_or(AppError::Internal)?,
    };
    Ok(Json(success(data, cursor)?))
}

pub(super) async fn run(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<RunDetailResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let id: RunId = parse(&run_id)?;
    let limit = query.limit()?;
    let endpoint = &entry.auth.endpoint;
    let after = match query.position(endpoint)? {
        None => None,
        Some(PagePosition::Run {
            context,
            run_id,
            session_key,
        }) => Some(SessionPosition {
            context: context.try_into()?,
            run_id: parse(&run_id)?,
            session_key: parse(&session_key)?,
        }),
        _ => return Err(AppError::CursorExpired.into()),
    };
    let Snapshot { value, cursor } = entry.store.read_run(actor, id, after, limit).await?;
    let sessions = value
        .sessions
        .iter()
        .map(|row| {
            Ok(SessionSummary {
                session_key: SessionKeyDto::new(row.session_key.as_str())
                    .ok_or(AppError::Internal)?,
                parent_session_key: row
                    .parent
                    .as_ref()
                    .and_then(|id| SessionKeyDto::new(id.as_str())),
                provider: provider(row.provider),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let next = value
        .next_session
        .map(|pos| {
            endpoint.encode_page(PagePosition::Run {
                context: pos.context.into(),
                run_id: pos.run_id.to_string(),
                session_key: pos.session_key.to_string(),
            })
        })
        .transpose()?;
    let data = RunDetail {
        review_id: ReviewIdDto::new(value.lead.review_id().as_str()).ok_or(AppError::Internal)?,
        run_id: RunIdDto::new(value.lead.run_id().as_str()).ok_or(AppError::Internal)?,
        lead_session_key: SessionKeyDto::new(value.lead.session_key().as_str())
            .ok_or(AppError::Internal)?,
        sessions: Page::new(sessions, next).ok_or(AppError::Internal)?,
    };
    Ok(Json(success(data, cursor)?))
}

pub(super) async fn resolve(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Query(query): Query<SelectionQuery>,
) -> Result<Json<ReviewSelectionResponse>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let review_id = parse(&query.review_id)?;
    let run_id = query.run_id.as_deref().map(parse).transpose()?;
    let Snapshot { value, cursor } = entry
        .store
        .resolve_selection(actor, review_id, run_id)
        .await?;
    let review_id = ReviewIdDto::new(value.review_id().as_str()).ok_or(AppError::Internal)?;
    let data = if let Some(lead) = value.lead() {
        ReviewSelection::WithLead {
            review_id,
            run_id: RunIdDto::new(lead.run_id().as_str()).ok_or(AppError::Internal)?,
            lead_session_key: SessionKeyDto::new(lead.session_key().as_str())
                .ok_or(AppError::Internal)?,
        }
    } else {
        ReviewSelection::WithoutLead {
            review_id,
            run_id: (),
            lead_session_key: (),
        }
    };
    Ok(Json(success(data, cursor)?))
}

pub(super) fn success<T>(data: T, cursor: EventCursor) -> Result<Success<T>, AppError> {
    Ok(Success {
        data,
        cursor: EventCursorDto::new(cursor.get()).ok_or(AppError::Internal)?,
    })
}

pub(super) fn project_review(item: &QueueItem) -> Result<MergeRequestRow, AppError> {
    let source = &item.source;
    let mr_state = match source.state {
        MergeRequestState::Open => MergeRequestStatus::Open,
        MergeRequestState::Closed => MergeRequestStatus::Closed,
        MergeRequestState::Merged => MergeRequestStatus::Merged,
    };
    Ok(MergeRequestRow {
        review_id: ReviewIdDto::new(item.review_id.as_str()).ok_or(AppError::Internal)?,
        project_id: ProjectIdDto::new(source.identity.project_id().get())
            .ok_or(AppError::Internal)?,
        project_path: source.project_path.clone(),
        iid: MergeRequestIidDto::new(source.identity.iid().get()).ok_or(AppError::Internal)?,
        title: source.title.clone(),
        tickets: Vec::new(),
        author: None,
        source_sha: GitShaDto::new(source.head_sha.as_str()).ok_or(AppError::Internal)?,
        mr_state,
        queue_state: if source.state == MergeRequestState::Open {
            QueueStatus::NeedsReview
        } else {
            QueueStatus::Archive
        },
        updated_at: UtcTimestampDto::from_unix_milliseconds(source.updated_at_ms)
            .ok_or(AppError::Internal)?,
        draft: false,
        own: false,
        our_comments: count(0)?,
        unread_replies: count(0)?,
        personal_approval: None,
        registered_lead_count: count(item.registered_lead_count)?,
        run_stage: None,
        reason_kind: None,
        has_agent_questions: false,
    })
}

pub(super) fn parse<T: std::str::FromStr>(value: &str) -> Result<T, AppError> {
    value.parse().map_err(|_| AppError::InvalidRequest)
}

fn provider(value: HostProvider) -> HostProviderDto {
    match value {
        HostProvider::Codex => HostProviderDto::Codex,
        HostProvider::Claude => HostProviderDto::Claude,
    }
}

fn count(value: u64) -> Result<SafeCountDto, AppError> {
    SafeCountDto::new(value).ok_or(AppError::Internal)
}

impl PageQuery {
    fn limit(&self) -> Result<u16, AppError> {
        let limit = self.limit.unwrap_or(50);
        (1..=MAX_QUEUE_PAGE)
            .contains(&limit)
            .then_some(limit)
            .ok_or(AppError::InvalidRequest)
    }

    fn position(&self, endpoint: &ServiceEndpoint) -> Result<Option<PagePosition>, AppError> {
        self.cursor
            .as_deref()
            .map(|value| endpoint.decode_page(value))
            .transpose()
    }
}

impl ServiceEndpoint {
    fn encode_page(&self, position: PagePosition) -> Result<PageCursorDto, AppError> {
        let mut bytes = serde_json::to_vec(&position).map_err(|_| AppError::Internal)?;
        let mut mac = self.cursor_mac();
        mac.update(b"reviewq-page-v1");
        mac.update(&bytes);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        PageCursorDto::new(URL_SAFE_NO_PAD.encode(bytes)).ok_or(AppError::Internal)
    }

    fn decode_page(&self, value: &str) -> Result<PagePosition, AppError> {
        if value.len() > 8192 {
            return Err(AppError::CursorExpired);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| AppError::CursorExpired)?;
        let (data, proof) = bytes
            .split_at_checked(bytes.len().checked_sub(32).ok_or(AppError::CursorExpired)?)
            .ok_or(AppError::CursorExpired)?;
        let mut mac = self.cursor_mac();
        mac.update(b"reviewq-page-v1");
        mac.update(data);
        mac.verify_slice(proof)
            .map_err(|_| AppError::CursorExpired)?;
        serde_json::from_slice(data).map_err(|_| AppError::CursorExpired)
    }
}

impl From<PageContext> for Context {
    fn from(context: PageContext) -> Self {
        Self {
            generation: context.generation.get(),
            authority: context.authority,
            limit: context.limit,
        }
    }
}

impl TryFrom<Context> for PageContext {
    type Error = AppError;

    fn try_from(context: Context) -> Result<Self, AppError> {
        Ok(Self {
            generation: context
                .generation
                .try_into()
                .map_err(|_| AppError::CursorExpired)?,
            authority: context.authority,
            limit: context.limit,
        })
    }
}
