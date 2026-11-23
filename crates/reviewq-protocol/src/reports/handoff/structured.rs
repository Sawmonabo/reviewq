use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StructuredReport {
    #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 1)))]
    pub version: u8,
    pub scope: ReportScope,
    #[cfg_attr(feature = "schema", schemars(length(max = 50)))]
    pub findings: Vec<StructuredFinding>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReportScope {
    FullReview,
    SelectedThreads,
    Amendment,
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
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 384)))]
    pub title: String,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 64)))]
    pub severity: String,
    pub blocking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<FindingLocation>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
    pub consequence: String,
    pub evidence: FindingBasis,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingLocation {
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 512)))]
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
    pub line: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FindingBasis {
    pub kind: FindingBasisKind,
    #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 2048)))]
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FindingBasisKind {
    Observed,
    Gap,
}
