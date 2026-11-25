//! Completes the executing follow-up batch a published version closes, after checking that
//! its assessments cover the frozen batch.

use std::collections::{BTreeMap, BTreeSet};

use reviewq_app::access::{AppError, FieldError};
use reviewq_app::events::ResourceKey;
use reviewq_app::followups::ResultKind;
use reviewq_app::reports::{Assessment, FindingConclusion, FindingEdit, ReportEdits};
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, params};

use super::reconciliation;
use super::work::FrozenManifest;
use crate::StoreError;
use crate::connection::WriteTx;

/// The private manifest frozen when the batch started.
#[expect(
    dead_code,
    reason = "K2 reads the frozen batch for the publish snapshot"
)]
pub(crate) fn frozen(db: &Connection, batch: &str) -> Result<FrozenManifest, StoreError> {
    let source: String = db.query_row(
        "SELECT private_manifest FROM followup_batches WHERE batch_id=?1",
        [batch],
        |row| row.get(0),
    )?;
    serde_json::from_str(&source).map_err(|_| AppError::Internal.into())
}

/// The finding IDs one "Include" error names before counting the rest.
const MAX_NAMED_IDS: usize = 20;

/// The finding IDs each batch discussion tracks, across its observations.
fn tracked(manifest: &FrozenManifest) -> BTreeMap<&str, BTreeSet<&str>> {
    let mut tracked = BTreeMap::<&str, BTreeSet<&str>>::new();
    for item in &manifest.observations {
        tracked
            .entry(&item.discussion_id)
            .or_default()
            .extend(item.finding_ids.iter().map(String::as_str));
    }
    tracked
}

/// One assessment per batch discussion, each covering the finding IDs its discussion tracks.
#[expect(dead_code, reason = "K2 checks the closing call's assessments")]
pub(crate) fn assessment_errors(
    manifest: &FrozenManifest,
    declared: &[Assessment],
) -> Vec<FieldError> {
    let expected = tracked(manifest);
    let mut assessed = BTreeSet::new();
    let mut errors = Vec::new();
    for (index, assessment) in declared.iter().enumerate() {
        let discussion = assessment.discussion_id.as_str();
        let path = format!("/assessments/{index}");
        let Some(ids) = expected.get(discussion) else {
            errors.push(FieldError::new(
                format!("{path}/discussionId"),
                format!("{discussion} is not a discussion of the selected-thread batch."),
            ));
            continue;
        };
        if !assessed.insert(discussion) {
            errors.push(FieldError::new(
                format!("{path}/discussionId"),
                "ID is already used in this collection.",
            ));
            continue;
        }
        let missing: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| !assessment.finding_ids.iter().any(|actual| actual == id))
            .collect();
        if !missing.is_empty() {
            // Name a bounded number of IDs so one error stays small.
            let more = missing.len().saturating_sub(MAX_NAMED_IDS);
            let mut named = missing[..missing.len().min(MAX_NAMED_IDS)].join(", ");
            if more > 0 {
                named.push_str(&format!(" and {more} more"));
            }
            errors.push(FieldError::new(
                format!("{path}/findingIds"),
                format!("Include {named} for discussion {discussion}."),
            ));
        }
    }
    for discussion in expected.keys() {
        if !assessed.contains(discussion) {
            errors.push(FieldError::new(
                "/assessments",
                format!("Add an assessment for discussion {discussion}."),
            ));
        }
    }
    errors
}

/// Records the batch's result and assessments, moves it from executing to completed and
/// reconciles the earlier observations it covers. `Conflict` when the batch is no longer
/// executing; nothing is written then.
#[expect(dead_code, reason = "K2 completes the batch inside the publish commit")]
pub(crate) fn complete(
    tx: &mut WriteTx<'_>,
    lead: &LeadTarget,
    kind: ResultKind,
    assessments: &[Assessment],
    manifest: &FrozenManifest,
    receipt: &str,
    time: i64,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    let review = lead.review_id().as_str();
    let run = lead.run_id().as_str();
    db.execute(
        "INSERT INTO followup_results(review_id,batch_id,run_id,receipt_id,kind,checked_sha,completed_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
         params![review,manifest.batch_id,run,receipt,kind.to_string(),manifest.source_sha,time],
    )?;
    for assessment in assessments {
        db.prepare_cached(
            "INSERT INTO followup_assessments(review_id,batch_id,discussion_id,conclusion,reason,next_action)
             VALUES(?1,?2,?3,?4,?5,?6)",
        )?.execute(params![review,manifest.batch_id,assessment.discussion_id,assessment.conclusion.to_string(),
             assessment.reason,assessment.next_action.to_string()])?;
        for finding in &assessment.finding_ids {
            db.prepare_cached(
                "INSERT INTO followup_assessment_findings(review_id,batch_id,discussion_id,finding_id)
                 VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",
            )?.execute(params![review,manifest.batch_id,assessment.discussion_id,finding])?;
        }
    }
    let changed=db.execute(
        "UPDATE followup_batches SET state='completed',completed_at_ms=?2,holds_slot=0,hold_reason=NULL
         WHERE batch_id=?1 AND authority_verified=1 AND state='executing'",
        params![manifest.batch_id,time],
    )?;
    if changed != 1 {
        return Err(AppError::Conflict.into());
    }
    reconciliation::accept(db, review, &manifest.batch_id)?;
    tx.emit(&[ResourceKey::Followup(Some(lead.review_id().clone()))])?;
    Ok(())
}

/// Each outcome's discussion links name batch discussions that track its finding, and each
/// linked discussion's assessment covers the finding. An outcome without links is linked to
/// every batch discussion that tracks its finding. Each assessment must also agree with the
/// findings this version retires; see [`conclusion_errors`].
#[expect(dead_code, reason = "K2 checks the outcomes against the assessments")]
pub(crate) fn edit_errors(
    db: &Connection,
    run: &str,
    manifest: &FrozenManifest,
    edits: &ReportEdits,
    assessments: &[Assessment],
) -> Result<Vec<FieldError>, StoreError> {
    let tracked = tracked(manifest);
    let tracks = |discussion: &str, finding: &str| {
        tracked
            .get(discussion)
            .is_some_and(|ids| ids.contains(finding))
    };
    let mut existing = db.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM report_findings WHERE run_id=?1 AND receipt_id=?2 AND finding_id=?3)",
    )?;
    let mut errors = Vec::new();
    let mut findings = BTreeSet::new();
    for (index, edit) in edits.findings.iter().enumerate() {
        let finding = edit.finding_id.as_str();
        let known: bool = existing
            .query_row(params![run, manifest.report_receipt_id, finding], |row| {
                row.get(0)
            })?;
        if known {
            findings.insert(finding);
        }
        for discussion in linked(&tracked, edit) {
            let covered = assessments.iter().any(|assessment| {
                assessment.discussion_id == discussion
                    && assessment.finding_ids.iter().any(|id| id == finding)
            });
            let message = if !tracked.contains_key(discussion) {
                format!("{discussion} is not a discussion of the selected-thread batch.")
            } else if known && !tracks(discussion, finding) {
                format!("Discussion {discussion} does not track {finding}.")
            } else if !covered {
                format!(
                    "The assessment of discussion {discussion} must list {finding} in findingIds."
                )
            } else {
                continue;
            };
            errors.push(FieldError::new(
                format!("/findingChanges/{index}/discussionIds"),
                message,
            ));
        }
    }
    errors.extend(conclusion_errors(&tracked, edits, &findings, assessments));
    Ok(errors)
}

/// The discussions an outcome concerns: its explicit links, or else every batch discussion
/// that tracks its finding.
fn linked<'a>(
    tracked: &BTreeMap<&'a str, BTreeSet<&str>>,
    edit: &'a FindingEdit,
) -> BTreeSet<&'a str> {
    if edit.discussion_ids.is_empty() {
        tracked
            .iter()
            .filter(|(_, ids)| ids.contains(edit.finding_id.as_str()))
            .map(|(discussion, _)| *discussion)
            .collect()
    } else {
        edit.discussion_ids.iter().map(String::as_str).collect()
    }
}

/// A discussion's assessment agrees with the findings this version retires: retiring when
/// the version retires every finding the discussion tracks, and retiring or
/// `partly_addressed` when it retires only some of them. Only an outcome for one of
/// `findings`, the frozen version's findings, retires one; restating the outcome of a
/// finding an earlier version retired does not. Acceptance requires the frozen version to
/// be the run's head.
fn conclusion_errors(
    tracked: &BTreeMap<&str, BTreeSet<&str>>,
    edits: &ReportEdits,
    findings: &BTreeSet<&str>,
    assessments: &[Assessment],
) -> Vec<FieldError> {
    let retired: BTreeSet<&str> = edits
        .findings
        .iter()
        .filter(|edit| {
            edit.conclusion.retires_finding() && findings.contains(edit.finding_id.as_str())
        })
        .map(|edit| edit.finding_id.as_str())
        .collect();
    let mut errors = Vec::new();
    for (index, assessment) in assessments.iter().enumerate() {
        let discussion = assessment.discussion_id.as_str();
        let Some(ids) = tracked.get(discussion) else {
            continue;
        };
        if ids.is_disjoint(&retired) {
            continue;
        }
        let conclusion = assessment.conclusion;
        let message = if ids.is_subset(&retired) {
            if conclusion.retires_finding() {
                continue;
            }
            format!(
                "This version retires every finding discussion {discussion} tracks, so use fixed, clarified_withdrawn or superseded."
            )
        } else {
            if conclusion.retires_finding() || conclusion == FindingConclusion::PartlyAddressed {
                continue;
            }
            format!(
                "This version retires some of the findings discussion {discussion} tracks, so use partly_addressed, fixed, clarified_withdrawn or superseded."
            )
        };
        errors.push(FieldError::new(
            format!("/assessments/{index}/conclusion"),
            message,
        ));
    }
    errors
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use reviewq_app::followups::NextAction;
    use reviewq_app::reports::{Assessment, FindingConclusion, FindingEdit, ReportEdits};

    use super::conclusion_errors;

    #[test]
    fn a_thread_retires_one_of_its_two_findings_as_partly_addressed() {
        let tracked = BTreeMap::from([("D1", BTreeSet::from(["F-01", "F-02"]))]);
        let fixed = || ReportEdits {
            findings: vec![FindingEdit {
                finding_id: "F-01".into(),
                conclusion: FindingConclusion::Fixed,
                reason: "The retry is bounded.".into(),
                checked_sha: None,
                discussion_ids: Vec::new(),
            }],
        };
        let assessment = |conclusion| {
            vec![Assessment {
                discussion_id: "D1".into(),
                conclusion,
                reason: "F-01 is fixed; F-02 is still present.".into(),
                finding_ids: vec!["F-01".into(), "F-02".into()],
                next_action: NextAction::WaitingAuthor,
            }]
        };
        let known = BTreeSet::from(["F-01", "F-02"]);
        let partly = assessment(FindingConclusion::PartlyAddressed);
        assert!(conclusion_errors(&tracked, &fixed(), &known, &partly).is_empty());
        let present = assessment(FindingConclusion::StillPresent);
        let errors = conclusion_errors(&tracked, &fixed(), &known, &present);
        assert_eq!(
            errors
                .iter()
                .map(|error| error.path.as_str())
                .collect::<Vec<_>>(),
            ["/assessments/0/conclusion"]
        );
        let mut both = fixed();
        both.findings.push(FindingEdit {
            finding_id: "F-02".into(),
            ..both.findings[0].clone()
        });
        let partly = assessment(FindingConclusion::PartlyAddressed);
        assert_eq!(conclusion_errors(&tracked, &both, &known, &partly).len(), 1);
        // F-01 is not a finding of the frozen version, as when an earlier version retired it,
        // so restating it as fixed retires nothing and F-02 stays present.
        let mut restated = fixed();
        restated.findings.push(FindingEdit {
            finding_id: "F-02".into(),
            conclusion: FindingConclusion::StillPresent,
            ..restated.findings[0].clone()
        });
        let known = BTreeSet::from(["F-02"]);
        assert!(conclusion_errors(&tracked, &restated, &known, &present).is_empty());
    }
}
