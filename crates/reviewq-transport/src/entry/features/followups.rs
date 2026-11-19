mod commands;
pub(in crate::entry) use commands::{execute, project_receipt, selection};

pub(super) use commands::{configure, pause, resume, review_now, save_preferences};

use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::Response;
use reviewq_app::access::AppError;
use reviewq_app::followups::{
    self as app, AssessmentPosition, MAX_ASSESSMENTS, MAX_RESULTS, ResultPosition,
};
use reviewq_app::reports::FindingConclusion;
use reviewq_domain::identity::{GitSha, Revision, RunId};
use reviewq_protocol::followups as dto;
use reviewq_protocol::reports::FindingConclusion as ConclusionDto;
use reviewq_protocol::{
    GitShaDto, Page, PageCursorDto, ResourceIdDto, RevisionDto, RunIdDto, SubmissionIdDto, Success,
    UtcTimestampDto,
};
use serde::{Deserialize, Serialize};

use crate::entry::error::{ApiPath as Path, ApiQuery as Query};
use crate::entry::pages::{count, parse, success};
use crate::entry::{ApiError, Entry};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageQuery {
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AssessmentQuery {
    limit: Option<u16>,
    cursor: Option<String>,
    through_batch_id: Option<ResourceIdDto>,
}

#[derive(Deserialize, Serialize)]
struct ResultCursor(i64, String);

#[derive(Deserialize, Serialize)]
struct AssessmentCursor(String, i64, String);

pub(super) async fn preferences(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
) -> Result<Json<Success<dto::Preferences>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let value = entry.store.read_followup_preferences(actor).await?;
    Ok(Json(success(
        project_preferences(value.value)?,
        value.cursor,
    )?))
}

pub(super) async fn enrollment(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
) -> Result<Json<Success<dto::Enrollment>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let stored = entry.store.read_followups(actor, parse(&review)?).await?;
    Ok(Json(success(
        project_enrollment(stored.value)?,
        stored.cursor,
    )?))
}

pub(in crate::entry) fn project_enrollment(
    value: app::Enrollment,
) -> Result<dto::Enrollment, AppError> {
    Ok(dto::Enrollment {
        preferences: project_preferences(value.preferences)?,
        monitor: value.monitor.map(monitor).transpose()?,
        batches: value
            .batches
            .into_iter()
            .map(batch)
            .collect::<Result<_, _>>()?,
        latest_completed_result: value.latest.map(result).transpose()?,
        unviewed_batch_count: count(value.unviewed_batches)?,
    })
}

pub(super) async fn results(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<Success<Page<dto::ResultSummary>>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let limit = query.limit.unwrap_or(MAX_RESULTS);
    let scope = format!("browser-followup-results:{limit}:{review}");
    let endpoint = &entry.auth.endpoint;
    let after = query
        .cursor
        .as_deref()
        .map(|token| {
            let ResultCursor(time, batch_id) = endpoint.decode_cursor(scope.as_bytes(), token)?;
            Ok::<_, AppError>(ResultPosition {
                completed_at_ms: time,
                batch_id,
            })
        })
        .transpose()?;
    let stored = entry
        .store
        .read_followup_results(actor, parse(&review)?, after, limit)
        .await?;
    let next = stored
        .value
        .has_more
        .then(|| {
            let item = stored.value.items.last().ok_or(AppError::Internal)?;
            let token = endpoint.encode_cursor(
                scope.as_bytes(),
                &ResultCursor(item.completed_at_ms, item.batch_id.clone()),
            )?;
            cursor(token)
        })
        .transpose()?;
    let items = stored
        .value
        .items
        .into_iter()
        .map(result)
        .collect::<Result<_, _>>()?;
    let page = Page::new(items, next).ok_or(AppError::Internal)?;
    Ok(Json(success(page, stored.cursor)?))
}

pub(super) async fn assessments(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    Query(query): Query<AssessmentQuery>,
) -> Result<Response, ApiError> {
    const MAX_PAGE_BYTES: usize = 512 * 1024;
    let actor = entry.auth.browser_actor(&headers, false)?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=MAX_ASSESSMENTS).contains(&limit) {
        return Err(AppError::InvalidRequest.into());
    }
    let scope = format!("browser-followup-assessments:{limit}:{review}");
    let endpoint = &entry.auth.endpoint;
    let after = query
        .cursor
        .as_deref()
        .map(|token| {
            let AssessmentCursor(through_batch_id, ordinal, discussion_id) =
                endpoint.decode_cursor(scope.as_bytes(), token)?;
            Ok::<_, AppError>(AssessmentPosition {
                through_batch_id,
                ordinal,
                discussion_id,
            })
        })
        .transpose()?;
    let stored = entry
        .store
        .read_followup_assessments(
            actor,
            parse(&review)?,
            query.through_batch_id.map(|id| id.as_str().to_owned()),
            after,
            limit,
        )
        .await?;
    let through = stored.value.through_batch_id.map(resource).transpose()?;
    let items = stored
        .value
        .items
        .into_iter()
        .map(|item| {
            let ordinal = item.ordinal;
            Ok::<_, AppError>((assessment(item)?, ordinal))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut len = items.len();
    loop {
        let prefix = &items[..len];
        let next_cursor = if stored.value.has_more || len < items.len() {
            let (last, ordinal) = prefix.last().ok_or(AppError::PayloadTooLarge)?;
            let id = through.as_ref().ok_or(AppError::Internal)?;
            let token = endpoint.encode_cursor(
                scope.as_bytes(),
                &AssessmentCursor(
                    id.as_str().to_owned(),
                    *ordinal,
                    last.discussion_id.as_str().to_owned(),
                ),
            )?;
            Some(cursor(token)?)
        } else {
            None
        };
        let value = success(
            dto::AssessmentPage {
                items: prefix.iter().map(|(item, _)| item).collect(),
                next_cursor,
                through_batch_id: through.clone(),
            },
            stored.cursor,
        )?;
        let bytes = serde_json::to_vec(&value).map_err(|_| AppError::Internal)?;
        if bytes.len() <= MAX_PAGE_BYTES {
            return Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(bytes))
                .map_err(|_| AppError::Internal.into());
        }
        if len <= 1 {
            return Err(AppError::PayloadTooLarge.into());
        }
        len /= 2;
    }
}

fn project_preferences(value: app::Preferences) -> Result<dto::Preferences, AppError> {
    Ok(dto::Preferences {
        automatic: value.automatic,
        auto_review_concurrency: value.concurrency,
        revision: revision(value.revision)?,
    })
}

fn monitor(value: app::Monitor) -> Result<dto::Monitor, AppError> {
    let selections = value
        .selections
        .into_iter()
        .map(|item| {
            Ok::<_, AppError>(dto::SelectedThread {
                original_note_id: resource(item.original_note_id)?,
                discussion_id: item.discussion_id.map(resource).transpose()?,
                reviewer_id: item.reviewer_id.map(resource).transpose()?,
                selected: item.selected,
                version: item.version.map(resource).transpose()?,
                resolved: item.resolved,
                finding_ids: item
                    .finding_ids
                    .into_iter()
                    .map(resource)
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(dto::Monitor {
        run_id: value.run.map(run).transpose()?,
        generation: revision(value.generation)?,
        state: match value.state {
            app::MonitorState::Enabled => dto::MonitorState::Enabled,
            app::MonitorState::Paused => dto::MonitorState::Paused,
            app::MonitorState::Archived => dto::MonitorState::Archived,
        },
        policy_override: value.policy_override,
        binding_verified: value.binding_verified,
        reason: value.reason,
        updated_at: time(value.updated_at_ms)?,
        selections,
    })
}

fn batch(value: app::Batch) -> Result<dto::Batch, AppError> {
    Ok(dto::Batch {
        batch_id: resource(value.id)?,
        run_id: value.run.map(run).transpose()?,
        generation: revision(value.generation)?,
        state: match value.state {
            app::BatchState::Pending => dto::BatchState::Pending,
            app::BatchState::Reserved => dto::BatchState::Reserved,
            app::BatchState::Executing => dto::BatchState::Executing,
            app::BatchState::Completed => dto::BatchState::Completed,
            app::BatchState::Cancelled => dto::BatchState::Cancelled,
            app::BatchState::Reconciliation => dto::BatchState::Reconciliation,
        },
        member_count: count(value.member_count)?,
        authority_verified: value.verified,
        checked_sha: value.source_sha.map(sha).transpose()?,
        target_sha: value.target_sha.map(sha).transpose()?,
        submission_id: value
            .submission
            .map(|id| SubmissionIdDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        hold_reason: value.hold_reason,
        due_at: value.due_at_ms.map(time).transpose()?,
        first_observed_at: time(value.first_at_ms)?,
        latest_observed_at: time(value.latest_at_ms)?,
        completed_at: value.completed_at_ms.map(time).transpose()?,
    })
}

fn result(value: app::ResultSummary) -> Result<dto::ResultSummary, AppError> {
    Ok(dto::ResultSummary {
        batch_id: resource(value.batch_id)?,
        run_id: run(value.run)?,
        receipt_id: value
            .report
            .map(|receipt| resource(receipt.as_str()))
            .transpose()?,
        kind: match value.kind {
            app::ResultKind::Final => dto::ResultKind::Final,
            app::ResultKind::Unchanged => dto::ResultKind::Unchanged,
        },
        checked_sha: value.checked_sha.map(sha).transpose()?,
        completed_at: time(value.completed_at_ms)?,
    })
}

fn assessment(value: app::Assessment) -> Result<dto::Assessment, AppError> {
    Ok(dto::Assessment {
        batch_id: resource(value.batch_id)?,
        receipt_id: value
            .receipt_id
            .map(|id| resource(id.as_str()))
            .transpose()?,
        discussion_id: resource(value.discussion_id)?,
        conclusion: match value.conclusion {
            FindingConclusion::Fixed => ConclusionDto::Fixed,
            FindingConclusion::ClarifiedWithdrawn => ConclusionDto::ClarifiedWithdrawn,
            FindingConclusion::Superseded => ConclusionDto::Superseded,
            FindingConclusion::PartlyAddressed => ConclusionDto::PartlyAddressed,
            FindingConclusion::StillPresent => ConclusionDto::StillPresent,
            FindingConclusion::Unverified => ConclusionDto::Unverified,
        },
        reason: value.reason,
        next_action: match value.next_action {
            app::NextAction::Review => dto::NextAction::Review,
            app::NextAction::WaitingAuthor => dto::NextAction::WaitingAuthor,
        },
        finding_ids: value
            .finding_ids
            .into_iter()
            .map(resource)
            .collect::<Result<_, _>>()?,
    })
}

fn resource(value: impl Into<Box<str>>) -> Result<ResourceIdDto, AppError> {
    ResourceIdDto::new(value).ok_or(AppError::Internal)
}
fn revision(value: Revision) -> Result<RevisionDto, AppError> {
    RevisionDto::new(value.get()).ok_or(AppError::Internal)
}
fn run(value: RunId) -> Result<RunIdDto, AppError> {
    RunIdDto::new(value.as_str()).ok_or(AppError::Internal)
}
fn cursor(value: String) -> Result<PageCursorDto, AppError> {
    PageCursorDto::new(value).ok_or(AppError::Internal)
}
fn sha(value: GitSha) -> Result<GitShaDto, AppError> {
    GitShaDto::new(value.as_str()).ok_or(AppError::Internal)
}
fn time(value: i64) -> Result<UtcTimestampDto, AppError> {
    UtcTimestampDto::from_unix_milliseconds(value).ok_or(AppError::Internal)
}
