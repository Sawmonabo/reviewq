use serde::{Deserialize, Serialize};

use crate::{HistoryTurnIdDto, ResourceIdDto, RevisionDto, Success};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Preferences {
    pub revision: RevisionDto,
    pub auto_resume_codex_reviews: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SavePreferences {
    pub expected_revision: RevisionDto,
    pub auto_resume_codex_reviews: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CancelRecovery {
    pub expected_revision: RevisionDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InterruptRequest {
    pub expected_turn_id: HistoryTurnIdDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Episode {
    pub episode_id: ResourceIdDto,
    pub revision: RevisionDto,
    pub failed_turn_id: HistoryTurnIdDto,
    pub kind: Kind,
    pub state: State,
    pub reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Kind {
    UsageLimit,
    ModelCapacity,
    Access,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum State {
    Disabled,
    Waiting,
    Queued,
    Started,
    Uncertain,
    Cancelled,
    Complete,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InterruptReceipt {
    pub turn_id: HistoryTurnIdDto,
    pub state: InterruptState,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InterruptState {
    Interrupted,
    Rejected,
    Uncertain,
}

pub type RecoveryPreferencesResponse = Success<Preferences>;
pub type RecoveryStateResponse = Success<Option<Episode>>;
pub type CancelRecoveryResponse = Success<Episode>;
pub type InterruptResponse = Success<InterruptReceipt>;
