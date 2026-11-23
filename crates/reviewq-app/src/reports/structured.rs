use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::access::AppError;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StructuredReport {
    pub version: u8,
    pub scope: ReportScope,
    pub findings: Vec<StructuredFinding>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportScope {
    FullReview,
    SelectedThreads,
    Amendment,
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

impl StructuredReport {
    pub(super) fn validate(&self, expected: ReportScope) -> Result<(), AppError> {
        let mut ids = BTreeSet::new();
        if self.version != 1
            || self.scope != expected
            || self.findings.len() > 50
            || self.findings.iter().any(|finding| {
                !ids.insert(finding.id.as_str())
                    || !valid_id(&finding.id)
                    || !text(&finding.title, 384)
                    || !text(&finding.severity, 64)
                    || !text(&finding.details.consequence, 1024)
                    || !text(&finding.details.evidence.text, 2048)
                    || finding.details.location.as_ref().is_some_and(|location| {
                        !text(&location.path, 512)
                            || location.path.chars().any(char::is_control)
                            || location.line == Some(0)
                    })
            })
        {
            return Err(AppError::InvalidRequest);
        }
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    value.len() <= 64
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn text(value: &str, max: usize) -> bool {
    value.len() <= max
        && !value.trim().is_empty()
        && !value
            .chars()
            .any(|value| value.is_control() && !matches!(value, '\n' | '\r' | '\t'))
}
