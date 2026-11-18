//! Bounded visible page for one exact registered source.

use serde::Serialize;

use crate::Success;
use crate::UtcTimestampDto;
use crate::scalar::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, HistoryTurnIdDto, RunIdDto,
    SafeCountDto, SessionKeyDto,
};

/// Encoded JSON budget for one history page.
pub const MAX_HISTORY_PAGE_BYTES: usize = 256 * 1024;
const MAX_HISTORY_TEXT_BYTES: usize = 64 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistoryPage {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub generation: HistoryGenerationDto,
    pub availability: HistoryAvailability,
    pub reset: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "SafeCountDto"))]
    pub retry_after_ms: Option<SafeCountDto>,
    #[serde(flatten)]
    pub window: HistoryWindow,
}

pub type HistoryPageResponse = Success<HistoryPage>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistoryWindow {
    #[cfg_attr(feature = "schema", schemars(length(max = 100)))]
    items: Vec<HistoryEntry>,
    next_cursor: Option<HistoryCursorDto>,
    previous_cursor: Option<HistoryCursorDto>,
}

impl HistoryWindow {
    pub fn new(
        items: Vec<HistoryEntry>,
        next_cursor: Option<HistoryCursorDto>,
        previous_cursor: Option<HistoryCursorDto>,
    ) -> Option<Self> {
        (items.len() <= 100).then_some(Self {
            items,
            next_cursor,
            previous_cursor,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HistoryAvailability {
    Available,
    Partial,
    Unavailable,
}

/// A visible native entry. Provider timestamps can be unknown.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistoryEntry {
    pub entry_id: HistoryEntryIdDto,
    pub turn_id: Option<HistoryTurnIdDto>,
    pub timestamp: Option<UtcTimestampDto>,
    pub parts: Vec<HistoryPart>,
}

/// Ordered visible content for the first Codex reader.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HistoryPart {
    UserPrompt { text: HistoryText },
    AssistantText { text: HistoryText },
}

/// Visible text bounded by its exact decoded UTF-8 byte length.
#[derive(Debug, Serialize)]
#[serde(transparent)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistoryText(
    #[cfg_attr(feature = "schema", schemars(schema_with = "history_text_schema"))] Box<str>,
);

impl HistoryText {
    pub fn new(value: impl Into<Box<str>>) -> Option<Self> {
        let value = value.into();
        (value.len() <= MAX_HISTORY_TEXT_BYTES).then_some(Self(value))
    }
}

#[cfg(feature = "schema")]
fn history_text_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "pattern": r"^[^\ud800-\udfff]*$",
        "maxUtf8Bytes": MAX_HISTORY_TEXT_BYTES,
    })
}
