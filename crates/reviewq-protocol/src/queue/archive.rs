use serde::Serialize;

use crate::{Page, SafeCountDto, Success, UpstreamIdDto, UtcTimestampDto};

use super::{RunCard, SprintPair};

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ArchiveDate {
    Decision,
    Report,
    Followup,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ArchiveMembership {
    Verified,
    Partial,
    Unknown,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArchiveBoard {
    pub id: UpstreamIdDto,
    pub label: String,
    pub available: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArchiveItem {
    #[serde(flatten)]
    pub card: RunCard,
    pub latest_matching_review_at: Option<UtcTimestampDto>,
    pub date_source: Option<ArchiveDate>,
    pub sprint_pairs: Vec<SprintPair>,
    pub membership: ArchiveMembership,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ArchivePage {
    #[serde(flatten)]
    pub page: Page<ArchiveItem>,
    pub total_archived: SafeCountDto,
    pub total_matching: SafeCountDto,
    pub boards: Vec<ArchiveBoard>,
}

pub type ArchivePageResponse = Success<ArchivePage>;

/// Compares a caller's timestamp with evidence stored at millisecond precision.
pub fn parse_archive_bound(value: &str) -> Option<i64> {
    let timestamp = value.parse::<jiff::Timestamp>().ok()?;
    let millis = timestamp.as_millisecond();
    if jiff::Timestamp::from_millisecond(millis).ok()? == timestamp {
        Some(millis)
    } else {
        None
    }
}
