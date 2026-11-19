use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reviewq_app::access::AppError;
use reviewq_app::events::Snapshot;
use reviewq_app::hosts::HostPhase;
use reviewq_app::queue::{
    MAX_QUEUE_PAGE, MergeRequestState, PageContext, QueueItem, RunPosition, SessionPosition,
};
use reviewq_app::settings::ActiveSource;
use reviewq_app::sources::{SourceFailure, SourceSync};
use reviewq_domain::identity::{EventCursor, ReviewId, RunId};
use reviewq_domain::session::HostProvider;
use reviewq_protocol::queue::{
    CompactTicket, FocusedLaunch, MergeRequestRow, MergeRequestStatus, QueueStatus, QueueSync,
    QueueSyncMode, ReviewOverview, ReviewOverviewResponse, ReviewSelection,
    ReviewSelectionResponse, RunStage, RunSummary,
};
use reviewq_protocol::{
    CheckpointIdDto, ErrorCode, ErrorDto, EventCursorDto, GitShaDto, HostProviderDto,
    MergeRequestIidDto, Page, PageCursorDto, ProjectIdDto, ResourceIdDto, ReviewIdDto, RevisionDto,
    RunDetail, RunDetailResponse, RunIdDto, SafeCountDto, SessionKeyDto, SessionSummary, Success,
    TicketKeyDto, UpstreamIdDto, UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use super::error::{ApiPath as Path, ApiQuery as Query};
use super::{ApiError, Entry, ServiceEndpoint};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageQuery {
    pub(super) limit: Option<u16>,
    pub(super) cursor: Option<String>,
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
pub(super) struct Context {
    generation: u64,
    authority: [u8; 32],
    limit: u16,
    projection: Option<i64>,
}

pub(super) fn source_failure(failure: SourceFailure) -> ErrorDto {
    let (code, message, retryable) = match failure {
        SourceFailure::Unavailable => (
            ErrorCode::Unavailable,
            "Could not reach the configured review source.",
            true,
        ),
        SourceFailure::TooLarge => (
            ErrorCode::PayloadTooLarge,
            "The review source exceeded the refresh limit.",
            false,
        ),
        SourceFailure::Interrupted => (
            ErrorCode::Unavailable,
            "The previous refresh was interrupted.",
            true,
        ),
    };
    ErrorDto {
        code,
        message: message.into(),
        retryable,
    }
}

pub(super) fn queue_sync(active: &ActiveSource, sync: SourceSync) -> QueueSync {
    let error = sync.error.map(source_failure);
    QueueSync {
        mode: if active.collection_enabled {
            QueueSyncMode::Connected
        } else if active.configured {
            QueueSyncMode::Disconnected
        } else {
            QueueSyncMode::Setup
        },
        active: sync.active,
        stale: active.configured
            && (!active.collection_enabled || sync.last_success_at_ms.is_none() || error.is_some()),
        last_attempt_at: sync
            .last_attempt_at_ms
            .and_then(UtcTimestampDto::from_unix_milliseconds),
        last_success_at: sync
            .last_success_at_ms
            .and_then(UtcTimestampDto::from_unix_milliseconds),
        error,
    }
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
    let Snapshot { value, cursor } = entry
        .store
        .read_review(actor.clone(), id.clone(), after, limit)
        .await?;
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
                lead_name: row.lead_name.clone(),
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
    let attention = super::features::decisions::project_attention(&value.frontier)?;
    let checkpoint = URL_SAFE_NO_PAD.encode(super::control::random()?);
    entry.checkpoints.remember(
        checkpoint.clone(),
        actor,
        id,
        value.settings_revision,
        value.frontier,
    )?;
    let data = ReviewOverview {
        review: project_review(&value.item)?,
        our_discussions: value
            .our_discussions
            .map(|value| {
                Ok::<_, AppError>(reviewq_protocol::queue::DiscussionCounts {
                    total: count(u64::from(value.total))?,
                    open: value.open.map(|n| count(u64::from(n))).transpose()?,
                })
            })
            .transpose()?,
        web_url: value.item.source.web_url()?,
        decision_revision: RevisionDto::new(value.item.decision.revision.get())
            .ok_or(AppError::Internal)?,
        decision: Some(super::features::decisions::project(&value.item.decision)?),
        attention: Some(attention),
        settings_revision: RevisionDto::new(value.settings_revision.get())
            .ok_or(AppError::Internal)?,
        checkpoint_id: Some(CheckpointIdDto::new(checkpoint).ok_or(AppError::Internal)?),
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
    Ok(Json(read_run(&entry, actor, parse(&run_id)?, query).await?))
}

pub(super) async fn read_run(
    entry: &Entry,
    actor: reviewq_app::access::Actor,
    id: RunId,
    query: PageQuery,
) -> Result<RunDetailResponse, AppError> {
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
        _ => return Err(AppError::CursorExpired),
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
                native_id: ResourceIdDto::new(row.native_id.as_str()).ok_or(AppError::Internal)?,
                cwd: row.cwd.as_path().display().to_string(),
                metadata: session_metadata(&row.metadata),
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
        lead_native_id: ResourceIdDto::new(value.lead.binding().identity().native_id().as_str())
            .ok_or(AppError::Internal)?,
        lead_cwd: value.lead.binding().cwd().as_path().display().to_string(),
        lead_metadata: session_metadata(&value.lead_metadata),
        sessions: Page::new(sessions, next).ok_or(AppError::Internal)?,
        request_evidence: value
            .request_evidence
            .map(super::launch::project_request_evidence)
            .transpose()?,
        launch: value
            .launch
            .map(super::launch::project_run_launch)
            .transpose()?,
    };
    success(data, cursor)
}

fn session_metadata(
    value: &reviewq_app::hosts::NativeSessionMetadata,
) -> reviewq_protocol::NativeSessionMetadata {
    reviewq_protocol::NativeSessionMetadata {
        name: value.name.clone(),
        status: value.status.clone(),
        started_at: value
            .started_at_ms
            .and_then(UtcTimestampDto::from_unix_milliseconds),
        model: value.model.clone(),
        effort: value.effort.clone(),
    }
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
    let signals = &item.signals;
    let (run_stage, reason_kind) = match signals
        .stage_reason
        .as_deref()
        .and_then(|value| value.split_once('/'))
    {
        Some((stage, reason)) => {
            let stage = match stage {
                "archived" => RunStage::Archived,
                "working" => RunStage::Working,
                "waiting" => RunStage::Waiting,
                "needs_input" => RunStage::NeedsInput,
                _ => return Err(AppError::Internal),
            };
            (Some(stage), Some(reason.to_owned()))
        }
        None => (None, None),
    };
    Ok(MergeRequestRow {
        review_id: ReviewIdDto::new(item.review_id.as_str()).ok_or(AppError::Internal)?,
        project_id: ProjectIdDto::new(source.identity.project_id().get())
            .ok_or(AppError::Internal)?,
        project_path: source.project_path.clone(),
        iid: MergeRequestIidDto::new(source.identity.iid().get()).ok_or(AppError::Internal)?,
        title: source.title.clone(),
        tickets: item
            .tickets
            .iter()
            .map(|ticket| {
                Ok(CompactTicket {
                    key: TicketKeyDto::new(ticket.key.as_str()).ok_or(AppError::Internal)?,
                    title: ticket.title.clone(),
                })
            })
            .collect::<Result<_, AppError>>()?,
        author: source
            .author
            .as_ref()
            .map(|author| {
                Ok(reviewq_protocol::queue::NamedUpstreamId {
                    id: UpstreamIdDto::new(author.id).ok_or(AppError::Internal)?,
                    label: author.username.clone(),
                })
            })
            .transpose()?,
        source_sha: GitShaDto::new(source.head_sha.as_str()).ok_or(AppError::Internal)?,
        mr_state,
        queue_state: match signals.queue_state.as_str() {
            "archive" => QueueStatus::Archive,
            "needs_review" => QueueStatus::NeedsReview,
            "followup" => QueueStatus::Followup,
            "reviewed" => QueueStatus::Reviewed,
            "waiting_author" => QueueStatus::WaitingAuthor,
            _ => return Err(AppError::Internal),
        },
        updated_at: UtcTimestampDto::from_unix_milliseconds(source.updated_at_ms)
            .ok_or(AppError::Internal)?,
        draft: source.draft,
        own: signals.own,
        our_comments: count(signals.our_comments)?,
        unread_replies: count(signals.unread_replies)?,
        personal_approval: signals.personal_approval,
        registered_lead_count: count(item.registered_lead_count)?,
        run_stage,
        reason_kind,
        has_agent_questions: signals.has_questions,
        focused_run_id: signals
            .run
            .as_ref()
            .map(|id| RunIdDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        focus_session_key: signals
            .focus
            .as_ref()
            .map(|id| SessionKeyDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        focused_provider: signals.focused_provider.map(provider),
        focused_activity: signals.focused_activity.map(|phase| match phase {
            HostPhase::Idle => reviewq_protocol::queue::FocusedActivity::Idle,
            HostPhase::Working => reviewq_protocol::queue::FocusedActivity::Working,
            HostPhase::Blocked => reviewq_protocol::queue::FocusedActivity::Blocked,
            HostPhase::Failed => reviewq_protocol::queue::FocusedActivity::Failed,
        }),
        registered_providers: signals
            .registered_providers
            .iter()
            .copied()
            .map(provider)
            .collect(),
        unread_non_reply_count: count(signals.unread_non_reply_count)?,
        current_disposition: item
            .decision
            .disposition
            .filter(|_| item.decision.reviewed_sha.as_ref() == Some(&source.head_sha))
            .map(super::features::decisions::project_disposition),
        focused_lead_name: signals.focused_lead_name.clone(),
        focused_launch: signals.focused_launch.as_ref().map(|launch| FocusedLaunch {
            observed_model: launch.observed_model.clone(),
            observed_effort: launch.observed_effort.clone(),
            requested_model: launch.requested_model.clone(),
            requested_effort: launch.requested_effort.clone(),
        }),
        focused_report_reviewed_sha: signals
            .focused_report_reviewed_sha
            .as_ref()
            .map(|sha| GitShaDto::new(sha.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        full_mr_reviewed_sha: signals
            .full_mr_reviewed_sha
            .as_ref()
            .map(|sha| GitShaDto::new(sha.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        coverage: match signals.coverage.as_deref() {
            Some("full_mr") => Some(reviewq_protocol::reports::Coverage::FullMr),
            Some("selected_threads") => Some(reviewq_protocol::reports::Coverage::SelectedThreads),
            None => None,
            _ => return Err(AppError::Internal),
        },
        last_activity_at: signals
            .run
            .as_ref()
            .and_then(|_| UtcTimestampDto::from_unix_milliseconds(signals.last_activity_ms)),
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

pub(super) fn count(value: u64) -> Result<SafeCountDto, AppError> {
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
        PageCursorDto::new(self.encode_cursor(b"reviewq-page-v1", &position)?)
            .ok_or(AppError::Internal)
    }

    fn decode_page(&self, value: &str) -> Result<PagePosition, AppError> {
        self.decode_cursor(b"reviewq-page-v1", value)
    }
}

impl From<PageContext> for Context {
    fn from(context: PageContext) -> Self {
        Self {
            generation: context.generation.get(),
            authority: context.authority,
            limit: context.limit,
            projection: context.projection,
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
            projection: context.projection,
        })
    }
}
