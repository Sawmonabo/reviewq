//! Checkout-specific native catalog and review launch preparation.

use serde::{Deserialize, Serialize};

use crate::settings::Skill;
use crate::{
    GitShaDto, HostProviderDto, ObservedInputDto, ResourceIdDto, ReviewIdDto, RevisionDto,
    RunIdDto, SessionKeyDto, SubmissionIdDto, Success, UtcTimestampDto,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CatalogRequest {
    pub review_id: ReviewIdDto,
    pub provider: HostProviderDto,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PrepareRequest {
    pub review_id: ReviewIdDto,
    pub provider: HostProviderDto,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub invocation: Option<Invocation>,
    pub context: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StartRequest {
    pub review_id: ReviewIdDto,
    pub provider: HostProviderDto,
    pub expected_settings_revision: RevisionDto,
    pub source_sha: GitShaDto,
    pub submission_id: SubmissionIdDto,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub invocation: Invocation,
    pub context: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Invocation {
    Skill {
        name: String,
        path: Option<String>,
        args: Option<String>,
    },
    Custom {
        prompt: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Catalog {
    pub review_id: ReviewIdDto,
    pub provider: HostProviderDto,
    pub settings_revision: RevisionDto,
    pub checkout: String,
    pub default_skill: Option<Skill>,
    pub models: Vec<Model>,
    pub skills: Vec<AvailableSkill>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AvailableSkill {
    pub name: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Model {
    pub id: String,
    pub label: String,
    pub is_default: bool,
    pub default_effort: Option<String>,
    pub efforts: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Preview {
    pub review_id: ReviewIdDto,
    pub provider: HostProviderDto,
    pub settings_revision: RevisionDto,
    pub source_sha: GitShaDto,
    pub checkout: String,
    pub invocation: Invocation,
    pub context: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub prompt: String,
}

pub type CatalogResponse = Success<Catalog>;
pub type PreviewResponse = Success<Preview>;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RunLaunchDetails {
    pub source_sha: GitShaDto,
    pub state: State,
    pub invocation: Invocation,
    pub context: Option<String>,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub observed_model: Option<String>,
    pub observed_effort: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum State {
    Pending,
    Confirmed,
    Rejected,
    Uncertain,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Receipt {
    pub review_id: ReviewIdDto,
    pub submission_id: SubmissionIdDto,
    pub state: State,
    pub run_id: Option<RunIdDto>,
    pub session_key: Option<SessionKeyDto>,
    pub native_id: Option<ResourceIdDto>,
    pub native_job_id: Option<ResourceIdDto>,
    pub observed_input: Option<ObservedInputDto>,
    pub created_at: UtcTimestampDto,
    pub updated_at: UtcTimestampDto,
}

pub type ReceiptResponse = Success<Receipt>;
