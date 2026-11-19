use serde::Serialize;

use crate::{
    HistoryCursorDto, HistoryEntryIdDto, HistoryGenerationDto, RunIdDto, SafeCountDto,
    SessionKeyDto, Success, UtcTimestampDto,
};

use super::{HistoryAvailability, HistoryPart, HistoryText};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistoryNavigation {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub generation: HistoryGenerationDto,
    pub availability: HistoryAvailability,
    pub reset: bool,
    pub complete: bool,
    #[serde(flatten)]
    pub page: NavigationPage,
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum NavigationPage {
    Overview {
        total: SafeCountDto,
        groups: Vec<PromptGroup>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema", schemars(with = "HistoryCursorDto"))]
        next_window_cursor: Option<HistoryCursorDto>,
    },
    Previews {
        items: Vec<PromptPreview>,
        next_cursor: Option<HistoryCursorDto>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PromptGroup {
    pub first: SafeCountDto,
    pub last: SafeCountDto,
    pub cursor: HistoryCursorDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PromptPreview {
    pub ordinal: SafeCountDto,
    pub entry_id: HistoryEntryIdDto,
    pub timestamp: Option<UtcTimestampDto>,
    pub text: HistoryText,
    pub locator: HistoryCursorDto,
}

pub type HistoryNavigationResponse = Success<HistoryNavigation>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HistoryPlan {
    pub run_id: RunIdDto,
    pub session_key: SessionKeyDto,
    pub generation: HistoryGenerationDto,
    pub availability: HistoryAvailability,
    pub plan: Option<PlanEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "Vec<PlanTask>"))]
    pub tasks: Option<Vec<PlanTask>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanTask {
    pub id: String,
    pub title: String,
    pub status: PlanTaskStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PlanTaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanEntry {
    pub entry_id: HistoryEntryIdDto,
    pub part: HistoryPart,
}

pub type HistoryPlanResponse = Success<HistoryPlan>;
