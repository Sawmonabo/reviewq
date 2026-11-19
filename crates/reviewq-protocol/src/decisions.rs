//! Local review decisions and exact displayed checkpoint requests.

use serde::{Deserialize, Serialize};

use crate::{
    CheckpointIdDto, GitShaDto, ReviewIdDto, RevisionDto, SubmissionIdDto, UtcTimestampDto,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Disposition {
    Reviewed,
    WaitingAuthor,
    NeedsReview,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DecisionRequest {
    pub submission_id: SubmissionIdDto,
    pub expected_revision: RevisionDto,
    pub expected_settings_revision: RevisionDto,
    pub source_sha: GitShaDto,
    pub checkpoint_id: Option<CheckpointIdDto>,
    pub disposition: Disposition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CheckpointRequest {
    pub submission_id: SubmissionIdDto,
    pub expected_revision: RevisionDto,
    pub expected_settings_revision: RevisionDto,
    pub source_sha: GitShaDto,
    pub checkpoint_id: CheckpointIdDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DecisionView {
    pub disposition: Option<Disposition>,
    pub reviewed_sha: Option<GitShaDto>,
    pub reviewed_at: Option<UtcTimestampDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DecisionResult {
    pub review_id: ReviewIdDto,
    pub decision_revision: RevisionDto,
    #[serde(flatten)]
    pub decision: DecisionView,
    pub checkpoint_id: Option<CheckpointIdDto>,
}
