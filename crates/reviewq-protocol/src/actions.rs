//! Guarded publication state and exact targets.

use serde::Serialize;

use crate::{
    GitShaDto, HistoryEntryIdDto, HistoryTurnIdDto, ResourceIdDto, RevisionDto, RunIdDto,
    SubmissionIdDto, UtcTimestampDto, reports::ReportHashDto,
};

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Intent {
    Comments,
    Approval,
    Both,
    Cancel,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum State {
    Working,
    Checking,
    Completed,
    RetryWait,
    NeedsInput,
    Cancelled,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Evidence {
    Unknown,
    Absent,
    Confirmed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Approval {
    NotRequested,
    Unknown,
    Absent,
    Confirmed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Target {
    pub body_sha256: ReportHashDto,
    pub discussion_id: Option<ResourceIdDto>,
    pub evidence: Evidence,
    pub note_id: Option<ResourceIdDto>,
    pub observed_at: Option<UtcTimestampDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Action {
    pub action_id: ResourceIdDto,
    pub run_id: RunIdDto,
    pub request_entry_id: Option<HistoryEntryIdDto>,
    pub turn_id: Option<HistoryTurnIdDto>,
    pub call_id: Option<ResourceIdDto>,
    pub reviewed_sha: Option<GitShaDto>,
    pub authority_verified: bool,
    pub revision: RevisionDto,
    pub intent: Intent,
    pub state: State,
    pub targets: Vec<Target>,
    pub approval: Approval,
    pub approval_observed_at: Option<UtcTimestampDto>,
    pub checked_at: Option<UtcTimestampDto>,
    pub attempts: u8,
    pub pending_submission_id: Option<SubmissionIdDto>,
    pub last_executed_submission_id: Option<SubmissionIdDto>,
    pub last_executed_turn_id: Option<HistoryTurnIdDto>,
    pub next_check_at: Option<UtcTimestampDto>,
    pub reason: Option<String>,
}

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclareCommand {
    pub session_key: crate::SessionKeyDto,
    pub call_id: ResourceIdDto,
    pub turn_id: Option<HistoryTurnIdDto>,
    pub request_entry_id: Option<HistoryEntryIdDto>,
    pub declaration: Declaration,
}

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Declaration {
    pub intent: Intent,
    pub reviewed_sha: GitShaDto,
    #[serde(default)]
    pub comments: Vec<CommentTarget>,
}

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommentTarget {
    pub body_sha256: ReportHashDto,
    pub discussion_id: Option<ResourceIdDto>,
}

#[derive(Debug, serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileCommand {
    pub session_key: crate::SessionKeyDto,
    pub call_id: ResourceIdDto,
}
