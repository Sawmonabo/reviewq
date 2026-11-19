use serde::Serialize;

use crate::reports::ReportHashDto;
use crate::{
    HistoryEntryIdDto, HistoryTurnIdDto, Page, ResourceIdDto, SafeCountDto, SubmissionIdDto,
    UtcTimestampDto,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RecordedPurpose {
    User,
    ReportFeedback,
    CodexRecovery,
    ThreadFollowup,
    Publication,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RecordedState {
    Queued,
    Dispatching,
    Started,
    Rejected,
    Uncertain,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MessageAttachment {
    pub receipt_id: Option<ResourceIdDto>,
    pub report_hash: Option<ReportHashDto>,
    pub current_receipt_id: Option<ResourceIdDto>,
    pub quote: Option<String>,
    pub quote_offset: Option<SafeCountDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MessageHistoryItem {
    pub message_id: ResourceIdDto,
    pub submission_id: SubmissionIdDto,
    pub purpose: RecordedPurpose,
    pub state: RecordedState,
    pub outcome_detail: Option<String>,
    pub followup_batch_id: Option<ResourceIdDto>,
    pub text: String,
    pub attachment: Option<MessageAttachment>,
    pub native_turn_id: Option<HistoryTurnIdDto>,
    pub native_entry_id: Option<HistoryEntryIdDto>,
    pub service_admitted: bool,
    pub created_at: UtcTimestampDto,
    pub updated_at: UtcTimestampDto,
}

pub type MessageHistoryPage = Page<MessageHistoryItem>;
