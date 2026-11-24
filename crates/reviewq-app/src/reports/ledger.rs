//! Derives a version's active findings by ID from its parent, its findings ledger and
//! its authored level-three `F-` headings.

use std::collections::{BTreeMap, BTreeSet};

use reviewq_domain::identity::{GitSha, ReceiptId, ReportHash};

use super::{
    Declaration, Descriptor, FindingDetails, FindingEdit, HandoffFieldError, ReportEdits,
    SourceRange, StructuredReport,
};

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
/// revision as their checked SHA, else from the parent. The anchor is this report's
/// top-level heading. A parent finding the handoff neither lists, retires nor heads at the
/// top level is carried from its origin. A heading without a source span, one nested in a
/// list item, adds its ID, title and heading priority, with no blocking status, and leaves
/// a new finding unanchored.
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
                source,
                ..
            } => Some((
                id.as_str(),
                Heading {
                    title,
                    severity,
                    blocking: *blocking,
                    source: source.as_ref(),
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
            let span = heading.and_then(|heading| heading.source);
            let (anchor, evidence_sha256) = match (span, supplied, prior) {
                (Some(span), ..) => (Anchor::Heading(span.clone()), Some(span.hash.clone())),
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

/// Checks a final's findings ledger against the parent's active findings while the tool
/// call is open. An outcome must name a parent finding. A full review lists or gives an
/// outcome for every parent finding. Each level-three `F-` heading of a file report, at the
/// top level or nested in a list item, must be listed, given an outcome or carried;
/// `headings` is `None` when they are unknown.
/// Amendments, unchanged results and questions carry no ledger.
pub fn check(
    declaration: &Declaration,
    parent: &[ParentFinding],
    headings: Option<&[String]>,
    full_review: bool,
) -> Vec<HandoffFieldError> {
    let Declaration::Final {
        report: Some(report),
        edits,
        ..
    } = declaration
    else {
        return Vec::new();
    };
    let active: BTreeSet<&str> = parent.iter().map(|finding| finding.id.as_str()).collect();
    let listed: BTreeSet<&str> = report
        .findings
        .iter()
        .map(|finding| finding.id.as_str())
        .collect();
    let outcomes: BTreeSet<&str> = edits
        .findings
        .iter()
        .map(|edit| edit.finding_id.as_str())
        .collect();
    let mut errors = Vec::new();
    for (index, edit) in edits.findings.iter().enumerate() {
        if !active.contains(edit.finding_id.as_str()) {
            errors.push(HandoffFieldError::new(
                format!("/handoff/findingChanges/{index}/findingId"),
                format!(
                    "{} is not a finding of the last accepted report.",
                    edit.finding_id
                ),
            ));
        }
    }
    if full_review {
        for finding in parent {
            let id = finding.id.as_str();
            if !listed.contains(id) && !outcomes.contains(id) {
                errors.push(HandoffFieldError::new(
                    "/handoff/report/findings",
                    format!(
                        "{id} · {} was in the last accepted report. List it in report.findings or give its outcome in findingChanges.",
                        untagged(id, &finding.title)
                    ),
                ));
            }
        }
    }
    for id in headings.unwrap_or_default() {
        let id = id.as_str();
        if !listed.contains(id) && !outcomes.contains(id) && !active.contains(id) {
            errors.push(HandoffFieldError::new(
                "/handoff/source",
                format!("The report has heading {id}, which is missing from report.findings."),
            ));
        }
    }
    errors
}

/// A title without the leading ID that a heading-derived title repeats, such as
/// "F-02 Retry loop never ends".
fn untagged<'a>(id: &str, title: &'a str) -> &'a str {
    match title.strip_prefix(id) {
        Some(rest)
            if rest.starts_with(|next: char| next.is_whitespace() || ":.-–—".contains(next)) =>
        {
            let rest = rest
                .trim_start_matches(|next: char| next.is_whitespace() || ":.-–—".contains(next));
            if rest.is_empty() { title } else { rest }
        }
        _ => title,
    }
}

struct Heading<'a> {
    title: &'a String,
    severity: &'a Option<String>,
    blocking: Option<bool>,
    /// `None` for a heading nested in a list item, which the reader draws without a card.
    source: Option<&'a SourceRange>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Anchor, ParentFinding, check, derive};
    use crate::reports::{
        Declaration, Descriptor, FindingBasis, FindingBasisKind, FindingDetails, ReportEdits,
        RevisionEvidence, Source, StructuredFinding, StructuredReport, Verdict, index,
    };

    // BLITZ-592: a full re-review listed F-04 and silently dropped F-05 and F-06.
    #[test]
    fn a_full_review_names_each_parent_finding_it_leaves_out() {
        let parent = [
            ("F-04", "Retry reuses a spent key"),
            ("F-05", "Webhook signature is not verified"),
            ("F-06", "Audit log omits the actor"),
        ]
        .map(|(id, title)| ParentFinding {
            id: id.into(),
            title: title.into(),
            severity: Some("P1".into()),
            blocking: Some(true),
            details: None,
            evidence_sha256: None,
            origin: "receipt-591".parse().unwrap(),
        });
        let review = Declaration::Final {
            source: Source::Response,
            revision: RevisionEvidence::Exact("a".repeat(40).parse().unwrap()),
            verdict: Verdict::RequestChanges,
            report: Some(Arc::new(StructuredReport {
                version: 1,
                findings: vec![StructuredFinding {
                    id: "F-04".into(),
                    title: "Retry reuses a spent key".into(),
                    severity: "P1".into(),
                    blocking: true,
                    details: FindingDetails {
                        location: None,
                        consequence: "A retried payment charges twice.".into(),
                        evidence: FindingBasis {
                            kind: FindingBasisKind::Observed,
                            text: "The retry path reads the consumed key.".into(),
                        },
                        checked_sha: None,
                        evidence_hash: None,
                    },
                }],
            })),
            amends: None,
            edits: ReportEdits::default(),
            assessments: Vec::new(),
        };
        let errors = check(&review, &parent, None, true);
        let messages: Vec<_> = errors
            .iter()
            .map(|error| (error.path.as_str(), error.message.as_str()))
            .collect();
        assert_eq!(
            messages,
            [
                (
                    "/handoff/report/findings",
                    "F-05 · Webhook signature is not verified was in the last accepted report. List it in report.findings or give its outcome in findingChanges."
                ),
                (
                    "/handoff/report/findings",
                    "F-06 · Audit log omits the actor was in the last accepted report. List it in report.findings or give its outcome in findingChanges."
                ),
            ]
        );
    }

    // A report in its own format wrote F-01's ID in inline code, repeated F-01, said both
    // "Blocking: yes" and "Blocking: no" under it, and raised F-02 inside a numbered list.
    // The parse must not fail, F-01 takes the last Blocking line before its first labelled
    // field, as its card shows, and F-02, which the reader draws without a card, must stay
    // a tracked finding.
    #[test]
    fn a_nested_heading_and_a_repeated_id_stay_tracked() {
        let markdown = "# Review of !418

### `F-01` · P1 · Session token written to logs

Blocking: yes

Blocking: no

**Evidence:** The request logger writes the bearer header.

Blocking: yes

### F-01 · P1 · Session token written to logs

The retry path logs it again.

## Other notes

1. ### F-02 Retry loop never ends

   The loop has no attempt cap.
";
        let headings = index::parse(markdown).expect("within the index budget");
        let sections = headings
            .iter()
            .filter(|item| matches!(item, Descriptor::Section { .. }))
            .count();
        assert_eq!(sections, 4, "the repeated heading stays a plain section");
        let ids: Vec<String> = headings
            .iter()
            .filter_map(|item| match item {
                Descriptor::Finding { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        let review = Declaration::Final {
            source: Source::Response,
            revision: RevisionEvidence::Exact("a".repeat(40).parse().unwrap()),
            verdict: Verdict::RequestChanges,
            report: Some(Arc::new(StructuredReport {
                version: 1,
                findings: Vec::new(),
            })),
            amends: None,
            edits: ReportEdits::default(),
            assessments: Vec::new(),
        };
        let messages: Vec<_> = check(&review, &[], Some(&ids), true)
            .into_iter()
            .map(|error| error.message)
            .collect();
        assert_eq!(
            messages,
            [
                "The report has heading F-01, which is missing from report.findings.",
                "The report has heading F-02, which is missing from report.findings.",
            ]
        );
        let Declaration::Final {
            report: Some(report),
            edits,
            ..
        } = &review
        else {
            unreachable!()
        };
        let derived: Vec<_> = derive(Vec::new(), report, edits, None, &headings)
            .findings
            .into_iter()
            .map(|finding| {
                let anchored = matches!(finding.anchor, Anchor::Heading(_));
                (finding.id, finding.severity, finding.blocking, anchored)
            })
            .collect();
        assert_eq!(
            derived,
            [
                ("F-01".into(), Some("P1".into()), Some(false), true),
                ("F-02".into(), None, None, false),
            ]
        );
    }
}
