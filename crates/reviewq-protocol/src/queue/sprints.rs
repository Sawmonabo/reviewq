use serde::{Deserialize, Serialize};

use crate::{RevisionDto, Success, UpstreamIdDto};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SprintPair {
    pub board_id: UpstreamIdDto,
    pub sprint_id: UpstreamIdDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LoadSprintsRequest {
    pub expected_revision: RevisionDto,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 32)))]
    pub pairs: Vec<SprintPair>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LoadedSprints {
    pub settings_revision: RevisionDto,
    pub loaded_pairs: Vec<SprintPair>,
}

pub type LoadSprintsResponse = Success<LoadedSprints>;
