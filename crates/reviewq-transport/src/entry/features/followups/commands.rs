use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use reviewq_app::access::AppError;
use reviewq_app::followups::{
    MonitorCommand, MonitorRequest, MonitorState, Selection, UpdatePreferences,
    execute_monitor_command,
};
use reviewq_domain::identity::Revision;
use reviewq_protocol::followups as dto;
use reviewq_protocol::{ReviewIdDto, RevisionDto, SubmissionIdDto, Success};

use super::{project_preferences, resource, revision};
use crate::entry::error::{ApiBytes, ApiPath as Path};
use crate::entry::pages::{parse, success};
use crate::entry::{ApiError, Entry};

type Receipt = Json<Success<dto::MonitorReceipt>>;

pub(in crate::entry::features) async fn save_preferences(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    ApiBytes(bytes): ApiBytes,
) -> Result<Json<Success<dto::Preferences>>, ApiError> {
    let actor = entry.auth.browser_actor(&headers, true)?;
    let input: dto::PreferencesRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let stored = entry
        .store
        .save_preferences(
            actor,
            UpdatePreferences {
                submission: parse(input.submission_id.as_str())?,
                expected_revision: input_revision(input.expected_revision)?,
                automatic: input.automatic,
                concurrency: input.auto_review_concurrency,
            },
        )
        .await?;
    Ok(Json(success(
        project_preferences(stored.value)?,
        stored.cursor,
    )?))
}

pub(in crate::entry::features) async fn configure(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Receipt, ApiError> {
    let input: dto::ConfigureRequest =
        serde_json::from_slice(&bytes).map_err(|_| AppError::InvalidRequest)?;
    let state = match input.state {
        dto::EnrollmentState::Enabled => MonitorState::Enabled,
        dto::EnrollmentState::Paused => MonitorState::Paused,
    };
    let selections = input
        .threads
        .into_iter()
        .map(|thread| Selection {
            discussion_id: thread.discussion_id.as_str().to_owned(),
            original_note_ids: thread
                .original_note_ids
                .into_iter()
                .map(|id| id.get())
                .collect(),
            finding_ids: thread
                .finding_ids
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
        })
        .collect();
    respond(
        &entry,
        &headers,
        &review,
        input.submission_id,
        input.expected_generation,
        MonitorCommand::Configure {
            run: parse(input.run_id.as_str())?,
            state,
            selections,
        },
    )
    .await
}

pub(in crate::entry::features) async fn pause(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Receipt, ApiError> {
    transition(&entry, &headers, &review, &bytes, MonitorCommand::Pause).await
}

pub(in crate::entry::features) async fn resume(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Receipt, ApiError> {
    transition(&entry, &headers, &review, &bytes, MonitorCommand::Resume).await
}

pub(in crate::entry::features) async fn review_now(
    State(entry): State<Arc<Entry>>,
    headers: HeaderMap,
    Path(review): Path<String>,
    ApiBytes(bytes): ApiBytes,
) -> Result<Receipt, ApiError> {
    if !entry.sends_allowed {
        return Err(AppError::Forbidden.into());
    }
    transition(&entry, &headers, &review, &bytes, MonitorCommand::ReviewNow).await
}

async fn transition(
    entry: &Entry,
    headers: &HeaderMap,
    review: &str,
    bytes: &[u8],
    command: MonitorCommand,
) -> Result<Receipt, ApiError> {
    let input: dto::MonitorRequest =
        serde_json::from_slice(bytes).map_err(|_| AppError::InvalidRequest)?;
    respond(
        entry,
        headers,
        review,
        input.submission_id,
        input.expected_generation,
        command,
    )
    .await
}

async fn respond(
    entry: &Entry,
    headers: &HeaderMap,
    review: &str,
    submission: SubmissionIdDto,
    expected: Option<RevisionDto>,
    command: MonitorCommand,
) -> Result<Receipt, ApiError> {
    let actor = entry.auth.browser_actor(headers, true)?;
    let stored = execute_monitor_command(
        entry.store.as_ref(),
        entry.source.as_deref(),
        actor,
        MonitorRequest {
            review: parse(review)?,
            submission: parse(submission.as_str())?,
            expected_generation: expected.map(input_revision).transpose()?,
            command,
        },
    )
    .await?;
    let saved = stored.value;
    Ok(Json(success(
        dto::MonitorReceipt {
            review_id: ReviewIdDto::new(saved.review.as_str()).ok_or(AppError::Internal)?,
            submission_id: SubmissionIdDto::new(saved.submission.as_str())
                .ok_or(AppError::Internal)?,
            generation: revision(saved.generation)?,
            state: match saved.state {
                MonitorState::Enabled => dto::MonitorState::Enabled,
                MonitorState::Paused => dto::MonitorState::Paused,
                MonitorState::Archived => dto::MonitorState::Archived,
            },
            batch_id: saved.batch_id.map(resource).transpose()?,
        },
        stored.cursor,
    )?))
}

fn input_revision(value: RevisionDto) -> Result<Revision, AppError> {
    Revision::try_from(value.get()).map_err(|_| AppError::InvalidRequest)
}
