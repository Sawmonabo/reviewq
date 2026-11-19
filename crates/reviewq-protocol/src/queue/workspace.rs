use serde::Serialize;

use crate::{Page, RunIdDto, SafeCountDto, Success};

use super::MergeRequestRow;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunCard {
    #[serde(flatten)]
    pub review: MergeRequestRow,
    pub run_ids: Vec<RunIdDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunCards {
    #[serde(flatten)]
    pub page: Page<RunCard>,
    pub stage_counts: StageCounts,
    pub total_matching: SafeCountDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StageCounts {
    pub needs_input: SafeCountDto,
    pub working: SafeCountDto,
    pub waiting: SafeCountDto,
    pub archived: SafeCountDto,
}

pub type RunCardsResponse = Success<RunCards>;
