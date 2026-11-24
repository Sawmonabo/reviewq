use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredReport {
    pub version: u8,
    pub findings: Vec<StructuredFinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredFinding {
    pub id: String,
    pub title: String,
    pub severity: String,
    pub blocking: bool,
    pub details: FindingDetails,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<FindingLocation>,
    pub consequence: String,
    pub evidence: FindingBasis,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_sha: Option<String>,
    #[serde(skip)]
    pub evidence_hash: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingLocation {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingBasis {
    pub kind: FindingBasisKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingBasisKind {
    Observed,
    Gap,
}
