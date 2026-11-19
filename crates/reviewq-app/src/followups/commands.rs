use std::collections::HashSet;

use reviewq_domain::identity::{MrIdentity, ReviewId, Revision, RunId, SubmissionId};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::sources::{ReviewSourceReader, WorkflowSnapshot};

use super::{MAX_SELECTED_NOTES, MonitorState, Preferences};

#[derive(Clone)]
pub struct Selection {
    pub discussion_id: String,
    pub original_note_ids: Vec<u64>,
    pub finding_ids: Vec<String>,
}

#[derive(Clone)]
pub enum MonitorCommand {
    Configure {
        run: RunId,
        state: MonitorState,
        selections: Vec<Selection>,
    },
    Pause,
    Resume,
    ReviewNow,
}

#[derive(Clone)]
pub struct MonitorRequest {
    pub review: ReviewId,
    pub submission: SubmissionId,
    pub expected_generation: Option<Revision>,
    pub command: MonitorCommand,
}

pub struct MonitorReceipt {
    pub review: ReviewId,
    pub submission: SubmissionId,
    pub generation: Revision,
    pub state: MonitorState,
    pub batch_id: Option<String>,
}

pub struct UpdatePreferences {
    pub submission: SubmissionId,
    pub expected_revision: Revision,
    pub automatic: Option<bool>,
    pub concurrency: Option<u8>,
}

pub trait FollowupCommands: Send + Sync {
    fn save_preferences(
        &self,
        actor: Actor,
        input: UpdatePreferences,
    ) -> AppFuture<'_, Snapshot<Preferences>>;
    fn read_monitor_replay(
        &self,
        actor: Actor,
        input: MonitorRequest,
    ) -> AppFuture<'_, Option<Snapshot<MonitorReceipt>>>;
    fn monitor_source(
        &self,
        actor: Actor,
        review: ReviewId,
    ) -> AppFuture<'_, (MrIdentity, Revision)>;
    fn commit_monitor(
        &self,
        actor: Actor,
        input: MonitorRequest,
        settings: Revision,
        observation: Option<WorkflowSnapshot>,
    ) -> AppFuture<'_, Snapshot<MonitorReceipt>>;
}

pub async fn execute_monitor_command(
    store: &(impl FollowupCommands + ?Sized),
    sources: Option<&dyn ReviewSourceReader>,
    actor: Actor,
    input: MonitorRequest,
) -> Result<Snapshot<MonitorReceipt>, AppError> {
    validate(&input.command)?;
    if let Some(previous) = store
        .read_monitor_replay(actor.clone(), input.clone())
        .await?
    {
        return Ok(previous);
    }
    let (identity, settings) = store
        .monitor_source(actor.clone(), input.review.clone())
        .await?;
    let observation = if matches!(input.command, MonitorCommand::Pause) {
        None
    } else {
        Some(
            sources
                .ok_or(AppError::Unavailable)?
                .workflow_snapshot(identity)
                .await?,
        )
    };
    store
        .commit_monitor(actor, input, settings, observation)
        .await
}

fn validate(command: &MonitorCommand) -> Result<(), AppError> {
    let MonitorCommand::Configure {
        state, selections, ..
    } = command
    else {
        return Ok(());
    };
    if *state == MonitorState::Archived || selections.len() > usize::from(MAX_SELECTED_NOTES) {
        return Err(AppError::InvalidRequest);
    }
    let mut notes = HashSet::new();
    let mut discussions = HashSet::new();
    for selection in selections {
        if selection.discussion_id.is_empty()
            || selection.discussion_id.len() > 256
            || selection.discussion_id.chars().any(char::is_control)
            || !discussions.insert(&selection.discussion_id)
            || selection.original_note_ids.is_empty()
            || selection.finding_ids.len() > 50
            || selection
                .finding_ids
                .iter()
                .any(|id| id.is_empty() || id.len() > 64 || id.chars().any(char::is_control))
        {
            return Err(AppError::InvalidRequest);
        }
        for id in &selection.original_note_ids {
            if *id == 0 || !notes.insert(id) || notes.len() > usize::from(MAX_SELECTED_NOTES) {
                return Err(AppError::InvalidRequest);
            }
        }
    }
    Ok(())
}
