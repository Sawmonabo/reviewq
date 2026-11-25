mod native;

pub(super) use native::{MAX_DECLARE_BODY_BYTES, declare, reconcile};

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::actions as app;
use reviewq_protocol::actions as dto;
use reviewq_protocol::{
    GitShaDto, HistoryEntryIdDto, HistoryTurnIdDto, ResourceIdDto, RevisionDto, RunIdDto,
    SubmissionIdDto, Success, UtcTimestampDto, reports::ReportHashDto,
};

use crate::entry::error::ApiPath as Path;
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

pub(super) async fn current(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(run): Path<String>,
) -> Result<Json<Success<Option<dto::Action>>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, false)?;
    let stored = entry.store.read_action(actor, parse(&run)?).await?;
    Ok(Json(success(
        stored.value.map(project).transpose()?,
        stored.cursor,
    )?))
}

pub(in crate::entry) fn project(value: app::Action) -> Result<dto::Action, AppError> {
    let time = |value| UtcTimestampDto::from_unix_milliseconds(value).ok_or(AppError::Internal);
    let submission = |id: reviewq_domain::identity::SubmissionId| {
        SubmissionIdDto::new(id.as_str()).ok_or(AppError::Internal)
    };
    let turn = |id: reviewq_domain::identity::NativeTurnId| {
        HistoryTurnIdDto::new(id.as_str()).ok_or(AppError::Internal)
    };
    let targets = value
        .targets
        .into_iter()
        .map(|target| {
            Ok::<_, AppError>(dto::Target {
                body_sha256: ReportHashDto::new(target.body_sha256.as_str())
                    .ok_or(AppError::Internal)?,
                discussion_id: target.discussion_id.map(resource).transpose()?,
                evidence: match target.evidence {
                    app::Evidence::Unknown => dto::Evidence::Unknown,
                    app::Evidence::Absent => dto::Evidence::Absent,
                    app::Evidence::Confirmed => dto::Evidence::Confirmed,
                },
                note_id: target.note_id.map(resource).transpose()?,
                observed_at: target.observed_at_ms.map(time).transpose()?,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(dto::Action {
        action_id: resource(value.id.as_str())?,
        run_id: RunIdDto::new(value.run.as_str()).ok_or(AppError::Internal)?,
        request_entry_id: value
            .request_entry
            .map(|id| HistoryEntryIdDto::new(id.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        turn_id: value.turn.map(turn).transpose()?,
        call_id: value.call_id.map(resource).transpose()?,
        reviewed_sha: value
            .reviewed_sha
            .map(|sha| GitShaDto::new(sha.as_str()).ok_or(AppError::Internal))
            .transpose()?,
        authority_verified: value.authority_verified,
        revision: RevisionDto::new(value.revision.get()).ok_or(AppError::Internal)?,
        intent: match value.intent {
            app::Intent::Comments => dto::Intent::Comments,
            app::Intent::Approval => dto::Intent::Approval,
            app::Intent::Both => dto::Intent::Both,
            app::Intent::Cancel => dto::Intent::Cancel,
        },
        state: match value.state {
            app::State::Working => dto::State::Working,
            app::State::Checking => dto::State::Checking,
            app::State::Completed => dto::State::Completed,
            app::State::RetryWait => dto::State::RetryWait,
            app::State::NeedsInput => dto::State::NeedsInput,
            app::State::Cancelled => dto::State::Cancelled,
        },
        targets,
        approval: match value.approval {
            app::Approval::NotRequested => dto::Approval::NotRequested,
            app::Approval::Unknown => dto::Approval::Unknown,
            app::Approval::Absent => dto::Approval::Absent,
            app::Approval::Confirmed => dto::Approval::Confirmed,
        },
        approval_observed_at: value.approval_observed_at_ms.map(time).transpose()?,
        checked_at: value.checked_at_ms.map(time).transpose()?,
        attempts: value.attempts,
        pending_submission_id: value.pending_submission.map(submission).transpose()?,
        last_executed_submission_id: value.last_executed_submission.map(submission).transpose()?,
        last_executed_turn_id: value.last_executed_turn.map(turn).transpose()?,
        next_check_at: value.next_check_at_ms.map(time).transpose()?,
        reason: value.reason,
    })
}

fn resource(value: impl Into<Box<str>>) -> Result<ResourceIdDto, AppError> {
    ResourceIdDto::new(value).ok_or(AppError::Internal)
}
