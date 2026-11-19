//! Shared input schema for ordinary Claude and Codex management tools.

use serde::Deserialize;

use crate::{MessageTextDto, SessionKeyDto, SubmissionIdDto};

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
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TranscriptRead {
    Page {
        cursor: Option<String>,
        limit: Option<u16>,
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
}
