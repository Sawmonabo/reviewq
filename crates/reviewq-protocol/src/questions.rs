//! Live native question forms and explicit human responses.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryTurnIdDto, ResourceIdDto, RunIdDto, SessionKeyDto,
    SubmissionIdDto, Success,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Answer {
    pub submission_id: SubmissionIdDto,
    pub turn_id: HistoryTurnIdDto,
    pub item_id: HistoryEntryIdDto,
    pub answers: BTreeMap<String, Values>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Values {
    pub answers: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Status {
    Pending,
    Submitting,
    Answered,
    Closed,
    Unavailable,
    Uncertain,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Mode {
    Native,
    Async,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Form {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub request_id: ResourceIdDto,
    pub turn_id: HistoryTurnIdDto,
    pub item_id: HistoryEntryIdDto,
    pub blocking: bool,
    pub mode: Mode,
    pub answerable: bool,
    pub status: Status,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub answered_elsewhere: bool,
    pub questions: Vec<Question>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "HistoryCursorDto"))]
    pub locator: Option<HistoryCursorDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<Choice>,
    pub is_other: bool,
    pub is_secret: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Choice {
    pub label: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "String"))]
    pub preview: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Forms {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub available: bool,
    pub items: Vec<Form>,
}

pub type FormsResponse = Success<Forms>;
pub type AnswerResponse = Success<Form>;
