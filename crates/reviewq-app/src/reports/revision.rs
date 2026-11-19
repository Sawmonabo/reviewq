//! Authored revisions and the exact evidence they are allowed to change.

use std::collections::BTreeSet;

use reviewq_domain::identity::{GitSha, ReportHash};

use crate::access::AppError;
use crate::labels::labels;

use super::{FindingConclusion, ReportReference, RevisionEvidence};

labels!(CorrectionKind {
    Presentation => "presentation",
    Evidence => "evidence",
});

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceTarget {
    Finding(String),
    Section(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceReference {
    pub target: EvidenceTarget,
    pub hash: ReportHash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Correction {
    pub base: ReportReference,
    pub kind: CorrectionKind,
    pub reason: String,
    pub finding_ids: Vec<String>,
    pub evidence: Vec<EvidenceReference>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FindingEdit {
    pub finding_id: String,
    pub conclusion: FindingConclusion,
    pub reason: String,
    pub checked_sha: GitSha,
    pub discussion_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionEdit {
    pub baseline_title: String,
    pub baseline_hash: ReportHash,
    pub replacement_title: Option<String>,
    pub conclusion: FindingConclusion,
    pub reason: String,
    pub discussion_ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReportEdits {
    pub correction: Option<Correction>,
    pub findings: Vec<FindingEdit>,
    pub sections: Vec<SectionEdit>,
}

impl ReportEdits {
    pub fn is_empty(&self) -> bool {
        self.correction.is_none() && self.findings.is_empty() && self.sections.is_empty()
    }

    pub(super) fn validate(
        &self,
        amends: Option<&ReportReference>,
        revision: &RevisionEvidence,
        scoped: bool,
    ) -> Result<(), AppError> {
        if self.is_empty() {
            return Ok(());
        }
        if amends.is_some()
            || self.findings.len() > 50
            || self.sections.len() > 20
            || self.correction.is_some() == scoped
        {
            return Err(AppError::InvalidRequest);
        }
        let mut ids = BTreeSet::new();
        if self.findings.iter().any(|edit| {
            !valid_id(&edit.finding_id, 64)
                || !ids.insert(edit.finding_id.as_str())
                || !reason(&edit.reason)
                || !matches!(revision, RevisionEvidence::Exact(sha) if sha == &edit.checked_sha)
                || !discussion_ids(&edit.discussion_ids, scoped)
        }) {
            return Err(AppError::InvalidRequest);
        }
        let mut section_keys = BTreeSet::new();
        if self.sections.iter().any(|edit| {
            !valid_id(&edit.baseline_title, 384)
                || !section_keys.insert((&edit.baseline_title, edit.baseline_hash.as_str()))
                || !reason(&edit.reason)
                || !discussion_ids(&edit.discussion_ids, scoped)
                || edit
                    .replacement_title
                    .as_ref()
                    .is_some_and(|value| !valid_id(value, 384))
                || (edit.replacement_title.is_none() && !edit.conclusion.retires_finding())
        }) {
            return Err(AppError::InvalidRequest);
        }
        if let Some(correction) = &self.correction {
            let mut affected = BTreeSet::new();
            if !reason(&correction.reason)
                || correction.finding_ids.is_empty()
                || correction.finding_ids.len() > 50
                || correction.evidence.is_empty()
                || correction.evidence.len() > 50
                || correction
                    .finding_ids
                    .iter()
                    .any(|id| !valid_id(id, 64) || !affected.insert(id.as_str()))
                || correction
                    .evidence
                    .iter()
                    .any(|record| match &record.target {
                        EvidenceTarget::Finding(id) => {
                            !valid_id(id, 64) || !affected.contains(id.as_str())
                        }
                        EvidenceTarget::Section(title) => !valid_id(title, 384),
                    })
                || match correction.kind {
                    CorrectionKind::Presentation => {
                        !self.findings.is_empty() || !self.sections.is_empty()
                    }
                    CorrectionKind::Evidence => ids != affected,
                }
            {
                return Err(AppError::InvalidRequest);
            }
        }
        Ok(())
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

fn discussion_ids(values: &[String], scoped: bool) -> bool {
    let unique = values.iter().collect::<BTreeSet<_>>();
    values.len() <= 10
        && values.len() == unique.len()
        && (!scoped || !values.is_empty())
        && values.iter().all(|id| valid_id(id, 256))
}

fn valid_id(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes && !value.chars().any(char::is_control)
}

fn reason(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1024
}
