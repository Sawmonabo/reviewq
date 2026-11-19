//! Checkout-specific native catalog and review launch preparation.

use serde::{Deserialize, Serialize};

use crate::settings::Skill;
use crate::{GitShaDto, HostProviderDto, ReviewIdDto, RevisionDto, Success};

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
    pub skills: Vec<Skill>,
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
