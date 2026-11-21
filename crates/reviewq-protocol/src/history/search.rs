use serde::Serialize;

use super::{HistoryAvailability, HistoryText};
use crate::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, RunIdDto, SafeCountDto,
    SessionKeyDto, Success,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistorySearchHit {
    pub entry_id: HistoryEntryIdDto,
    pub hit: HistoryCursorDto,
    pub before: HistoryText,
    pub matched: HistoryText,
    pub after: HistoryText,
    pub before_cursor: HistoryCursorDto,
    pub after_cursor: HistoryCursorDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistorySearchPage {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub generation: Option<HistoryGenerationDto>,
    pub availability: HistoryAvailability,
    #[cfg_attr(feature = "schema", schemars(length(max = 16)))]
    pub hits: Vec<HistorySearchHit>,
    pub next_cursor: Option<HistoryCursorDto>,
}

pub type HistorySearchResponse = Success<HistorySearchPage>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistorySearchDestination {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub entry_id: HistoryEntryIdDto,
    pub generation: HistoryGenerationDto,
    pub locator: HistoryCursorDto,
    pub content_ref: HistoryCursorDto,
    pub content_cursor: Option<HistoryCursorDto>,
    pub parts_cursor: Option<HistoryCursorDto>,
    pub native_index: Option<SafeCountDto>,
    pub tool_call_id: Option<HistoryEntryIdDto>,
    pub offset: SafeCountDto,
    pub start_utf16: SafeCountDto,
    pub end_utf16: SafeCountDto,
    pub markdown: bool,
    pub matched: HistoryText,
    pub ordinal: SafeCountDto,
}

pub type HistorySearchDestinationResponse = Success<HistorySearchDestination>;
