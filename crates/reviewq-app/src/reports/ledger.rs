//! Derives a version's active findings by ID from its parent, its findings ledger and
//! its authored level-three `F-` headings.

use std::collections::{BTreeMap, BTreeSet};

use reviewq_domain::identity::{GitSha, ReceiptId, ReportHash};

use super::{Descriptor, FindingDetails, FindingEdit, ReportEdits, SourceRange, StructuredReport};

/// An active finding of the parent version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParentFinding {
    pub id: String,
    pub title: String,
    pub severity: Option<String>,
    pub blocking: Option<bool>,
    pub details: Option<FindingDetails>,
    /// The stored hash of the row's anchored span, or of its authored card.
    pub evidence_sha256: Option<ReportHash>,
    /// The accepted receipt where the finding's card was last authored.
    pub origin: ReceiptId,
}

/// Where a derived finding's card is found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Anchor {
    /// This report's own `F-` heading.
    Heading(SourceRange),
    /// A parent finding this handoff neither lists, retires nor heads: the receipt where
    /// its card was last authored.
    Carried(ReceiptId),
    /// A listed finding without a heading in this report.
    None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveFinding {
    pub id: String,
    pub title: String,
    pub severity: Option<String>,
    pub blocking: Option<bool>,
    pub details: Option<FindingDetails>,
    /// The anchor's span hash. `None` for an unanchored ledger card, whose digest the
    /// store computes from the submitted card.
    pub evidence_sha256: Option<ReportHash>,
    pub anchor: Anchor,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Ledger {
    /// The new version's active findings, ordered by ID.
    pub findings: Vec<ActiveFinding>,
    /// Every outcome the handoff records, ordered by finding ID.
    pub changes: Vec<FindingEdit>,
}

/// Derives the next version's active findings.
///
/// A retiring outcome excludes its ID even when this report keeps its heading. Every other
/// ID of the parent, the ledger or the headings stays active. Fields come from the ledger,
/// then the parent, then the heading. Details come from the ledger, with the reviewed
/// revision as their checked SHA, else from the parent. The anchor is this report's heading.
/// A parent finding the handoff neither lists, retires nor heads is carried from its origin.
/// Headings without a source span are ignored.
pub fn derive(
    parent: Vec<ParentFinding>,
    report: &StructuredReport,
    edits: &ReportEdits,
    reviewed_sha: Option<&GitSha>,
    headings: &[Descriptor],
) -> Ledger {
    let retired: BTreeSet<&str> = edits
        .findings
        .iter()
        .filter(|edit| edit.conclusion.retires_finding())
        .map(|edit| edit.finding_id.as_str())
        .collect();
    let mut parent: BTreeMap<String, ParentFinding> = parent
        .into_iter()
        .map(|finding| (finding.id.clone(), finding))
        .collect();
    let ledger: BTreeMap<&str, _> = report
        .findings
        .iter()
        .map(|finding| (finding.id.as_str(), finding))
        .collect();
    let headings: BTreeMap<&str, Heading<'_>> = headings
        .iter()
        .filter_map(|item| match item {
            Descriptor::Finding {
                id,
                title,
                severity,
                blocking,
                source: Some(source),
                ..
            } => Some((
                id.as_str(),
                Heading {
                    title,
                    severity,
                    blocking: *blocking,
                    source,
                },
            )),
            _ => None,
        })
        .collect();
    let ids: BTreeSet<String> = parent
        .keys()
        .cloned()
        .chain(ledger.keys().map(|id| (*id).to_owned()))
        .chain(headings.keys().map(|id| (*id).to_owned()))
        .filter(|id| !retired.contains(id.as_str()))
        .collect();
    let findings = ids
        .into_iter()
        .map(|id| {
            let supplied = ledger.get(id.as_str()).copied();
            let prior = parent.remove(&id);
            let heading = headings.get(id.as_str());
            let title = supplied
                .map(|finding| finding.title.clone())
                .or_else(|| prior.as_ref().map(|finding| finding.title.clone()))
                .or_else(|| heading.map(|heading| heading.title.clone()))
                .unwrap_or_default();
            let severity = supplied
                .map(|finding| finding.severity.clone())
                .or_else(|| prior.as_ref().and_then(|finding| finding.severity.clone()))
                .or_else(|| heading.and_then(|heading| heading.severity.clone()));
            let blocking = supplied
                .map(|finding| finding.blocking)
                .or_else(|| prior.as_ref().and_then(|finding| finding.blocking))
                .or_else(|| heading.and_then(|heading| heading.blocking));
            let details = match supplied {
                Some(finding) => Some(FindingDetails {
                    checked_sha: reviewed_sha.map(|sha| sha.as_str().to_owned()),
                    ..finding.details.clone()
                }),
                None => prior.as_ref().and_then(|finding| finding.details.clone()),
            };
            let (anchor, evidence_sha256) = match (heading, supplied, prior) {
                (Some(heading), ..) => (
                    Anchor::Heading(heading.source.clone()),
                    Some(heading.source.hash.clone()),
                ),
                (None, None, Some(prior)) => (Anchor::Carried(prior.origin), prior.evidence_sha256),
                (None, ..) => (Anchor::None, None),
            };
            ActiveFinding {
                id,
                title,
                severity,
                blocking,
                details,
                evidence_sha256,
                anchor,
            }
        })
        .collect();
    let mut changes = edits.findings.clone();
    changes.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
    Ledger { findings, changes }
}

struct Heading<'a> {
    title: &'a String,
    severity: &'a Option<String>,
    blocking: Option<bool>,
    source: &'a SourceRange,
}
