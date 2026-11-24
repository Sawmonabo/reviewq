use serde::{Deserialize, Serialize};

#[cfg(feature = "schema")]
use super::limits;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StructuredReport {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    pub version: u8,
    #[cfg_attr(feature = "schema", schemars(length(max = limits::MAX_FINDINGS)))]
    pub findings: Vec<StructuredFinding>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StructuredFinding {
    #[cfg_attr(
        feature = "schema",
        schemars(regex(pattern = "^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$"))
    )]
    pub id: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_TITLE_BYTES)))]
    pub title: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_SEVERITY_BYTES)))]
    pub severity: String,
    pub blocking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<FindingLocation>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_CONSEQUENCE_BYTES)))]
    pub consequence: String,
    pub evidence: FindingBasis,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingLocation {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_PATH_BYTES)))]
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 4294967295_u32)))]
    pub line: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingBasis {
    pub kind: FindingBasisKind,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = limits::MAX_EVIDENCE_BYTES)))]
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FindingBasisKind {
    Observed,
    Gap,
}
