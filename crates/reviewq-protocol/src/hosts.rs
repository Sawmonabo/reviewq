use serde::{Deserialize, Deserializer, Serialize, de};

use crate::scalar::{
    HistoryEntryIdDto, HistoryTurnIdDto, ReviewIdDto, RunIdDto, SessionKeyDto, SubmissionIdDto,
};
use crate::{Page, Success, UtcTimestampDto};

const MAX_MESSAGE_CHARACTERS: usize = 12_000;

/// The browser body for POST /runs/{runId}/sessions/{sessionKey}/messages.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SendMessageRequest {
    pub submission_id: SubmissionIdDto,
    pub text: MessageTextDto,
}

/// Exact message text bounded in Unicode scalar values.
#[derive(Debug, Serialize)]
#[serde(transparent)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MessageTextDto(
    #[cfg_attr(feature = "schema", schemars(schema_with = "message_text_schema"))] Box<str>,
);

impl MessageTextDto {
    pub fn new(value: impl Into<Box<str>>) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()
            && value.chars().take(MAX_MESSAGE_CHARACTERS + 1).count() <= MAX_MESSAGE_CHARACTERS)
            .then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MessageTextDto {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Box::<str>::deserialize(deserializer)?).ok_or_else(|| {
            de::Error::custom("message must contain 1 to 12000 Unicode scalar values")
        })
    }
}

/// The addressed receipt for the message send and receipt lookup routes.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeliveryReceipt {
    pub submission_id: SubmissionIdDto,
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    #[serde(flatten)]
    pub state: DeliveryState,
    pub created_at: UtcTimestampDto,
    pub updated_at: UtcTimestampDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_ref: Option<DeliveryReportRef>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DeliveryReportRef {
    pub receipt_id: crate::ResourceIdDto,
    pub report_hash: crate::reports::ReportHashDto,
    pub current_receipt_id: Option<crate::ResourceIdDto>,
}

/// The snapshot returned by message send and receipt lookup.
pub type DeliveryReceiptResponse = Success<DeliveryReceipt>;

/// The known durable state of one message submission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DeliveryState {
    Queued { observed_input: () },
    Dispatching { observed_input: () },
    Started { observed_input: ObservedInputDto },
    Rejected { observed_input: () },
    Uncertain { observed_input: () },
}

/// The native input confirmed for this submission within the addressed run/session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ObservedInputDto {
    pub turn_id: HistoryTurnIdDto,
    pub entry_id: HistoryEntryIdDto,
}

/// Confirmed run detail, with the exact lead and bounded safe session summaries.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunDetail {
    pub review_id: ReviewIdDto,
    pub run_id: RunIdDto,
    pub lead_session_key: SessionKeyDto,
    pub sessions: Page<SessionSummary>,
}

pub type RunDetailResponse = Success<RunDetail>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SessionSummary {
    pub session_key: SessionKeyDto,
    pub parent_session_key: Option<SessionKeyDto>,
    pub provider: HostProviderDto,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HostProviderDto {
    Codex,
    Claude,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TerminalResult {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub provider: HostProviderDto,
    pub native_id: crate::ResourceIdDto,
    #[serde(flatten)]
    pub handoff: TerminalHandoff,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TerminalHandoff {
    Command { command: String, opened: bool },
    RestoreCheckout,
    ConfigureProvider,
    ForegroundClaude,
    InspectParent,
}

pub type TerminalResponse = Success<TerminalResult>;

#[cfg(feature = "schema")]
fn message_text_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "pattern": format!(r"^[^\ud800-\udfff]{{1,{MAX_MESSAGE_CHARACTERS}}}$"),
    })
}

mod messages;
pub use messages::{
    MessageAttachment, MessageHistoryItem, MessageHistoryPage, RecordedPurpose, RecordedState,
};
