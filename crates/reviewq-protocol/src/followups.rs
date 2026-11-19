//! Bounded selected-thread enrollment and outcome projections.

use serde::Serialize;

use crate::reports::FindingConclusion;
use crate::{
    GitShaDto, PageCursorDto, ResourceIdDto, RevisionDto, RunIdDto, SafeCountDto, SubmissionIdDto,
    UtcTimestampDto,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Preferences {
    pub automatic: bool,
    pub auto_review_concurrency: u8,
    pub revision: RevisionDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MonitorState {
    Enabled,
    Paused,
    Archived,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SelectedThread {
    pub original_note_id: ResourceIdDto,
    pub discussion_id: Option<ResourceIdDto>,
    pub reviewer_id: Option<ResourceIdDto>,
    pub selected: bool,
    pub version: Option<ResourceIdDto>,
    pub resolved: Option<bool>,
    pub finding_ids: Vec<ResourceIdDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Monitor {
    pub run_id: Option<RunIdDto>,
    pub generation: RevisionDto,
    pub state: MonitorState,
    pub policy_override: Option<bool>,
    pub binding_verified: bool,
    pub reason: Option<String>,
    pub updated_at: UtcTimestampDto,
    pub selections: Vec<SelectedThread>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum BatchState {
    Pending,
    Reserved,
    Executing,
    Completed,
    Cancelled,
    Reconciliation,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Batch {
    pub batch_id: ResourceIdDto,
    pub run_id: Option<RunIdDto>,
    pub generation: RevisionDto,
    pub state: BatchState,
    pub member_count: SafeCountDto,
    pub authority_verified: bool,
    pub checked_sha: Option<GitShaDto>,
    pub target_sha: Option<GitShaDto>,
    pub submission_id: Option<SubmissionIdDto>,
    pub hold_reason: Option<String>,
    pub due_at: Option<UtcTimestampDto>,
    pub first_observed_at: UtcTimestampDto,
    pub latest_observed_at: UtcTimestampDto,
    pub completed_at: Option<UtcTimestampDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ResultKind {
    Final,
    Unchanged,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ResultSummary {
    pub batch_id: ResourceIdDto,
    pub run_id: RunIdDto,
    pub receipt_id: Option<ResourceIdDto>,
    pub kind: ResultKind,
    pub checked_sha: Option<GitShaDto>,
    pub completed_at: UtcTimestampDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Enrollment {
    pub preferences: Preferences,
    pub monitor: Option<Monitor>,
    pub batches: Vec<Batch>,
    pub latest_completed_result: Option<ResultSummary>,
    pub unviewed_batch_count: SafeCountDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NextAction {
    Review,
    WaitingAuthor,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Assessment {
    pub batch_id: ResourceIdDto,
    pub receipt_id: Option<ResourceIdDto>,
    pub discussion_id: ResourceIdDto,
    pub conclusion: FindingConclusion,
    pub reason: String,
    pub next_action: NextAction,
    pub finding_ids: Vec<ResourceIdDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AssessmentPage<I = Assessment> {
    #[cfg_attr(feature = "schema", schemars(length(max = 500)))]
    pub items: Vec<I>,
    pub next_cursor: Option<PageCursorDto>,
    pub through_batch_id: Option<ResourceIdDto>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PreferencesRequest {
    pub submission_id: SubmissionIdDto,
    pub expected_revision: RevisionDto,
    pub automatic: Option<bool>,
    pub auto_review_concurrency: Option<u8>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EnrollmentState {
    Enabled,
    Paused,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Selection {
    pub discussion_id: ResourceIdDto,
    pub original_note_ids: Vec<crate::UpstreamIdDto>,
    #[serde(default)]
    pub finding_ids: Vec<ResourceIdDto>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigureRequest {
    pub submission_id: SubmissionIdDto,
    pub expected_generation: Option<RevisionDto>,
    pub run_id: RunIdDto,
    pub state: EnrollmentState,
    pub threads: Vec<Selection>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MonitorRequest {
    pub submission_id: SubmissionIdDto,
    pub expected_generation: Option<RevisionDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MonitorReceipt {
    pub review_id: crate::ReviewIdDto,
    pub submission_id: SubmissionIdDto,
    pub generation: RevisionDto,
    pub state: MonitorState,
    pub batch_id: Option<ResourceIdDto>,
}
