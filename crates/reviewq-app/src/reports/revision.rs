//! Outcomes an authored revision records for findings of the parent version.

use reviewq_domain::identity::GitSha;

use super::FindingConclusion;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingEdit {
    pub finding_id: String,
    pub conclusion: FindingConclusion,
    pub reason: String,
    /// The exact reviewed revision, or `None` when the revision is unknown.
    pub checked_sha: Option<GitSha>,
    pub discussion_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReportEdits {
    pub findings: Vec<FindingEdit>,
}

impl ReportEdits {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }
}

impl FindingConclusion {
    pub fn retires_finding(self) -> bool {
        matches!(
            self,
            Self::Fixed | Self::ClarifiedWithdrawn | Self::Superseded
        )
    }
}
