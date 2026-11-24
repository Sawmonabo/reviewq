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

/// The parent version a final's ledger builds on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Parent {
    /// The parent's active findings.
    pub findings: Vec<ParentFinding>,
    /// IDs a retiring outcome of this or an earlier version retired and the parent does not
    /// hold active. A struck heading for such an ID records history and anchors nothing;
    /// one that is not struck through raises the finding again.
    pub retired: BTreeSet<String>,
}

/// A level-three `F-` heading that a submit read from a file report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmittedHeading {
    pub id: String,
    /// The ID sits inside strikethrough or a `<del>`, `<s>` or `<strike>` tag, as a
    /// resolved finding's heading may. Such a heading never needs listing.
    pub struck: bool,
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
/// A retiring outcome excludes its ID even when this report keeps its heading. A struck
/// heading is a resolved card and adds no ID. It anchors a listed or active parent finding,
/// but never one the run retired in an earlier version: listing that ID re-raises it as a
/// new finding, unanchored unless a heading without strikethrough heads it. Every other ID
/// of the parent, the ledger or the unstruck headings stays active. Fields come from the
/// ledger, then the parent, then the heading. Details come from the ledger, with the
/// reviewed revision as their checked SHA, else from the parent. The anchor is this
/// report's top-level heading. A parent finding the handoff neither lists, retires nor
/// heads at the top level is carried from its origin. An unstruck heading without a source
/// span, one nested in a list item, adds its ID, title and heading priority, with no
/// blocking status, and leaves a new finding unanchored.
pub fn derive(
    parent: Parent,
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
    let Parent {
        findings: parent,
        retired: earlier,
    } = parent;
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
                struck,
                ..
            } => Some((
                id.as_str(),
                Heading {
                    title,
                    severity,
                    blocking: *blocking,
                    source: source.as_ref(),
                    struck: *struck,
                },
            )),
            _ => None,
        })
        .collect();
    let ids: BTreeSet<String> = parent
        .keys()
        .cloned()
        .chain(ledger.keys().map(|id| (*id).to_owned()))
        .chain(
            headings
                .iter()
                .filter(|(_, heading)| !heading.struck)
                .map(|(id, _)| (*id).to_owned()),
        )
        .filter(|id| !retired.contains(id.as_str()))
        .collect();
    let findings = ids
        .into_iter()
        .map(|id| {
            let supplied = ledger.get(id.as_str()).copied();
            let prior = parent.remove(&id);
            // A struck card records an earlier version's outcome for a retired ID, so it
            // cannot show the finding listed again; the index already preferred a plain one.
            let heading = headings
                .get(id.as_str())
                .filter(|heading| !(heading.struck && earlier.contains(&id)));
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
/// call is open. An outcome must name a parent finding, except a retiring outcome that
/// restates a finding the run retired before or one this report lists again. A full review
/// lists or gives an outcome for every parent finding. Each level-three `F-` heading of a
/// file report, at the top level or nested in a list item, must be listed, given an outcome
/// or carried, unless it is struck through: a struck heading is a resolved card, whatever
/// its ID, and never an error. `headings` is `None` when they are unknown. Amendments,
/// unchanged results and questions carry no ledger.
pub fn check(
    declaration: &Declaration,
    parent: &Parent,
    headings: Option<&[SubmittedHeading]>,
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
    let active: BTreeSet<&str> = parent
        .findings
        .iter()
        .map(|finding| finding.id.as_str())
        .collect();
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
        let id = edit.finding_id.as_str();
        if active.contains(id) {
            continue;
        }
        // Restating that an earlier version's retired finding is still resolved is harmless
        // and is recorded as given; only a keeping outcome needs the finding raised again.
        let message = match parent.retired.contains(id) {
            true if edit.conclusion.retires_finding() || listed.contains(id) => continue,
            true => format!(
                "{id} was retired in an earlier version. List it in report.findings to raise it again."
            ),
            false => format!("{id} is not a finding of the last accepted report."),
        };
        errors.push(HandoffFieldError::new(
            format!("/handoff/findingChanges/{index}/findingId"),
            message,
        ));
    }
    if full_review {
        for finding in &parent.findings {
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
    for heading in headings.unwrap_or_default() {
        let id = heading.id.as_str();
        if heading.struck || listed.contains(id) || outcomes.contains(id) || active.contains(id) {
            continue;
        }
        let message = match parent.retired.contains(id) {
            true => format!(
                "The report has heading {id}, which an earlier version retired. List it in report.findings if it is raised again, or restate its outcome in findingChanges."
            ),
            false => format!("The report has heading {id}, which is missing from report.findings."),
        };
        errors.push(HandoffFieldError::new("/handoff/source", message));
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
    /// `None` for a heading nested in a list item, which the reader draws as a ledger card.
    source: Option<&'a SourceRange>,
    struck: bool,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Anchor, Parent, ParentFinding, SubmittedHeading, check, derive};
    use crate::reports::{
        Declaration, Descriptor, FindingBasis, FindingBasisKind, FindingConclusion, FindingDetails,
        FindingEdit, ReportEdits, RevisionEvidence, Source, StructuredFinding, StructuredReport,
        Verdict, index,
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
        let parent = Parent {
            findings: parent.to_vec(),
            retired: Default::default(),
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
    // "Blocking: yes" and "Blocking: no" under it, raised F-02 inside a numbered list, gave
    // F-03 its severity and blocking status in one two-line paragraph, and struck F-04's
    // heading with `<del>` after an earlier version retired it as fixed. It struck F-05's
    // heading, a finding resolved before the run tracked it. It kept a struck heading for
    // F-06, also retired, then raised F-06 again under a plain heading without listing it,
    // listed F-07, another retired finding, as still present, listed F-08, retired in v3,
    // as P1 while keeping only its struck v3 heading, and restated that F-09, retired
    // before, is fixed.
    // The parse must not fail, F-01 takes the last Blocking line before its first labelled
    // field, as its card shows, and F-02, which the reader draws as a ledger card, must stay
    // a tracked finding. F-03 keeps P1 and blocking, and the struck F-04 and F-05 neither
    // need listing nor come back active. F-07 and F-09 need no change, and F-06 comes back
    // active from its plain heading, which the error asks the agent to list. F-08 is active
    // and unanchored, so its card is not the struck "fixed in v3" one.
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

### F-03 Webhook secret compared with ==

Severity: P1
Blocking: yes

### <del>F-04 · P3 · Stale comment</del> — fixed in v2

### ~~F-05 · P2 · Debug flag left on~~ — fixed before ReviewQ

### ~~F-06 · P2 · Token logged~~ — fixed in v3

### F-06 · P1 · Token logged again

Blocking: yes

**Evidence:** The retry path logs the bearer header again.

### ~~F-08 · P2 · Token logged~~ — fixed in v3

## Other notes

1. ### F-02 Retry loop never ends

   The loop has no attempt cap.
";
        let headings = index::parse(markdown).expect("within the index budget");
        let sections = headings
            .iter()
            .filter(|item| matches!(item, Descriptor::Section { .. }))
            .count();
        assert_eq!(sections, 10, "each repeated heading stays a plain section");
        let submitted: Vec<SubmittedHeading> = headings
            .iter()
            .filter_map(|item| match item {
                Descriptor::Finding { id, struck, .. } => Some(SubmittedHeading {
                    id: id.clone(),
                    struck: *struck,
                }),
                _ => None,
            })
            .collect();
        let ids: Vec<_> = submitted
            .iter()
            .map(|heading| (heading.id.as_str(), heading.struck))
            .collect();
        assert_eq!(
            ids,
            [
                ("F-01", false),
                ("F-03", false),
                ("F-04", true),
                ("F-05", true),
                ("F-06", false),
                ("F-08", true),
                ("F-02", false)
            ],
            "a struck heading keeps its ID, and a plain heading claims a repeated one"
        );
        let review = Declaration::Final {
            source: Source::Response,
            revision: RevisionEvidence::Exact("a".repeat(40).parse().unwrap()),
            verdict: Verdict::RequestChanges,
            report: Some(Arc::new(StructuredReport {
                version: 1,
                findings: [
                    (
                        "F-07",
                        "Webhook retries skip the signature check",
                        "A forged webhook is accepted on retry.",
                        "The retry handler calls process before verify.",
                    ),
                    (
                        "F-08",
                        "Token logged by the new audit hook",
                        "Session tokens reach the audit log.",
                        "The audit hook serializes the request headers.",
                    ),
                ]
                .map(|(id, title, consequence, evidence)| StructuredFinding {
                    id: id.into(),
                    title: title.into(),
                    severity: "P1".into(),
                    blocking: true,
                    details: FindingDetails {
                        location: None,
                        consequence: consequence.into(),
                        evidence: FindingBasis {
                            kind: FindingBasisKind::Observed,
                            text: evidence.into(),
                        },
                        checked_sha: None,
                        evidence_hash: None,
                    },
                })
                .into(),
            })),
            amends: None,
            edits: ReportEdits {
                findings: [
                    ("F-07", FindingConclusion::StillPresent),
                    ("F-09", FindingConclusion::Fixed),
                ]
                .map(|(id, conclusion)| FindingEdit {
                    finding_id: id.into(),
                    conclusion,
                    reason: "Checked again at this revision.".into(),
                    checked_sha: None,
                    discussion_ids: Vec::new(),
                })
                .into(),
            },
            assessments: Vec::new(),
        };
        let parent = Parent {
            findings: Vec::new(),
            retired: ["F-04", "F-06", "F-07", "F-08", "F-09"]
                .map(String::from)
                .into(),
        };
        let messages: Vec<_> = check(&review, &parent, Some(&submitted), true)
            .into_iter()
            .map(|error| error.message)
            .collect();
        assert_eq!(
            messages,
            [
                "The report has heading F-01, which is missing from report.findings.",
                "The report has heading F-03, which is missing from report.findings.",
                "The report has heading F-06, which an earlier version retired. List it in report.findings if it is raised again, or restate its outcome in findingChanges.",
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
        let derived: Vec<_> = derive(parent, report, edits, None, &headings)
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
                ("F-03".into(), Some("P1".into()), Some(true), true),
                ("F-06".into(), Some("P1".into()), Some(true), true),
                ("F-07".into(), Some("P1".into()), Some(true), false),
                ("F-08".into(), Some("P1".into()), Some(true), false),
            ]
        );
    }
}
