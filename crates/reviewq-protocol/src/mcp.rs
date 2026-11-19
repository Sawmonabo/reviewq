//! Shared input schema for ordinary Claude and Codex management tools.

use serde::{Deserialize, Serialize};

use crate::{
    GitShaDto, HistoryEntryIdDto, HostProviderDto, MessageTextDto, ResourceIdDto, RevisionDto,
    RunIdDto, SessionKeyDto, SubmissionIdDto,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ListReviews {
    pub filter: Option<ReviewFilter>,
    pub offset: Option<u32>,
    pub limit: Option<u16>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReviewFilter {
    All,
    Unstarted,
    Working,
    Waiting,
    NeedsInput,
    AgentQuestions,
    Archived,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GetReview {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub offset: Option<u32>,
    pub limit: Option<u16>,
    pub launch_submission_id: Option<SubmissionIdDto>,
    pub question_id: Option<ResourceIdDto>,
    pub report: Option<ReportRead>,
}

#[derive(Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReportRead {
    Summary {
        receipt_id: Option<ResourceIdDto>,
    },
    Section {
        receipt_id: Option<ResourceIdDto>,
        offset: Option<crate::SafeCountDto>,
        characters: Option<u32>,
    },
    Candidate {
        receipt_id: ResourceIdDto,
        offset: Option<crate::SafeCountDto>,
        characters: Option<u32>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReadTranscript {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub read: TranscriptRead,
}

#[derive(Deserialize)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TranscriptRead {
    Page {
        cursor: Option<String>,
        limit: Option<u16>,
        direction: Option<crate::history::Direction>,
        around_id: Option<HistoryEntryIdDto>,
        locator: Option<String>,
    },
    Content {
        #[serde(rename = "ref")]
        reference: String,
        cursor: Option<String>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SendReviewMessage {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub submission_id: SubmissionIdDto,
    pub text: MessageTextDto,
    pub finding: Option<crate::reports::FeedbackReference>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StartReview {
    pub target: String,
    pub provider: HostProviderDto,
    pub submission_id: SubmissionIdDto,
    pub expected_sha: Option<GitShaDto>,
    pub invocation: Option<crate::launch::Invocation>,
    pub context: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigureFollowups {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub run_id: Option<RunIdDto>,
    pub action: FollowupAction,
    pub submission_id: SubmissionIdDto,
    pub expected_generation: Option<RevisionDto>,
    pub threads: Option<Vec<crate::followups::Selection>>,
    pub state: Option<crate::followups::EnrollmentState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FollowupAction {
    Configure,
    Pause,
    Resume,
    ReviewNow,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AnswerReviewQuestion {
    pub target: String,
    pub session_key: Option<SessionKeyDto>,
    pub request_id: ResourceIdDto,
    pub submission_id: SubmissionIdDto,
    pub answers: std::collections::BTreeMap<String, crate::questions::Values>,
}
