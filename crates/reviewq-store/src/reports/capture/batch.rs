use std::collections::{BTreeMap, BTreeSet};

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::reports::{
    Assessment, Declaration, FindingConclusion, FindingEdit, HandoffFieldError, ReportEdits,
    ReportWork,
};
use reviewq_app::workflow::NativeCall;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::StoreError;
use crate::connection::WriteTx;
use crate::followups::work::FrozenManifest;
use crate::queue::parse;

#[derive(Serialize, Deserialize)]
struct StoredAssessment {
    discussion_id: String,
    conclusion: String,
    reason: String,
    finding_ids: Vec<String>,
    next_action: String,
}

pub(super) fn encode(values: &[Assessment]) -> Result<String, StoreError> {
    serde_json::to_string(
        &values
            .iter()
            .map(|a| StoredAssessment {
                discussion_id: a.discussion_id.clone(),
                conclusion: a.conclusion.to_string(),
                reason: a.reason.clone(),
                finding_ids: a.finding_ids.clone(),
                next_action: a.next_action.to_string(),
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|_| AppError::Internal.into())
}

pub(super) fn decode(source: &str) -> Result<Vec<Assessment>, StoreError> {
    let values: Vec<StoredAssessment> =
        serde_json::from_str(source).map_err(|_| AppError::Internal)?;
    values
        .into_iter()
        .map(|a| {
            Ok(Assessment {
                discussion_id: a.discussion_id,
                conclusion: parse(&a.conclusion)?,
                reason: a.reason,
                finding_ids: a.finding_ids,
                next_action: parse(&a.next_action)?,
            })
        })
        .collect()
}

pub(super) fn bound(db: &Connection, call: &NativeCall) -> Result<Option<String>, StoreError> {
    let root = call.root_turn_id.as_ref().filter(|root| {
        call.lead.binding().identity().provider() == reviewq_domain::session::HostProvider::Codex
            && *root != &call.turn_id
    });
    Ok(db.query_row(
        "SELECT b.batch_id FROM followup_batches b JOIN workflow_dispatch w ON w.batch_id=b.batch_id
         JOIN messages m ON m.message_id=w.delivery_id
         WHERE b.review_id=?1 AND b.run_id=?2 AND b.lead_session_key=?3 AND b.state='executing'
           AND b.authority_verified=1 AND b.native_turn_id=m.native_turn_id AND m.state='started'
           AND (
               b.native_turn_id=?4 AND b.continuation_turn_id IS NULL OR
               b.continuation_turn_id=?4 AND b.native_turn_id=?5 AND EXISTS(
                 SELECT 1 FROM recovery_episodes e JOIN messages resumed ON resumed.message_id=e.delivery_id
                 WHERE e.run_id=b.run_id AND e.lead_session_key=b.lead_session_key
                   AND e.root_turn_id=?5 AND e.resumed_turn_id=?4 AND e.state='started'
                   AND resumed.state='started' AND resumed.native_turn_id=?4
               ) OR
               b.native_turn_id=?5 AND EXISTS(
                   SELECT 1 FROM followup_monitors active WHERE active.review_id=b.review_id
                     AND active.run_id=b.run_id AND active.generation=b.generation
                     AND active.state='enabled' AND active.binding_verified=1
                     AND coalesce(active.policy_override,(SELECT automatic FROM followup_preferences WHERE singleton=1),0)=1
               ) AND EXISTS(
                   SELECT 1 FROM report_handoffs prior WHERE prior.batch_id=b.batch_id AND prior.form='needs_input'
                     AND prior.state='recorded' AND prior.source_generation=?6 AND prior.source_position<?7
                     AND (b.continuation_turn_id=?4 OR
                          b.hold_reason='followup_native_input_required' AND
                          prior.turn_id=coalesce(b.continuation_turn_id,b.native_turn_id))
               )
           )",
        params![call.lead.review_id().as_str(),call.lead.run_id().as_str(),
            call.lead.session_key().as_str(),call.turn_id.as_str(),root.map(|turn|turn.as_str()),
            call.source_generation,super::db_int(call.source_position)?],|row|row.get(0),
    ).optional()?)
}

pub(super) fn continue_on_turn(
    db: &Connection,
    batch: &str,
    call: &NativeCall,
) -> Result<bool, StoreError> {
    let updated = db.execute(
        "UPDATE followup_batches SET continuation_turn_id=?2,holds_slot=1,hold_reason=NULL
         WHERE batch_id=?1 AND state='executing' AND native_turn_id<>?2
           AND continuation_turn_id IS NOT ?2",
        params![batch, call.turn_id.as_str()],
    )?;
    Ok(updated == 1)
}

pub(super) fn action_turn(db: &Connection, call: &NativeCall) -> Result<bool, StoreError> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM workflow_dispatch w JOIN messages m ON m.message_id=w.delivery_id
         WHERE w.action_id IS NOT NULL AND m.run_id=?1 AND m.session_key=?2 AND m.native_turn_id=?3
           AND m.state='started')",
        params![call.lead.run_id().as_str(),call.lead.session_key().as_str(),call.turn_id.as_str()],
        |row|row.get(0),
    )?)
}

pub(super) fn manifest(
    db: &Connection,
    work: &ReportWork,
) -> Result<Option<FrozenManifest>, StoreError> {
    let expected: Option<String> = db.query_row(
        "SELECT batch_id FROM report_handoffs WHERE receipt_id=?1",
        [work.acknowledgement.receipt_id.as_str()],
        |row| row.get(0),
    )?;
    let Some(expected) = expected else {
        return Ok(None);
    };
    if bound(db, &work.call)?.as_deref() != Some(expected.as_str()) {
        return Err(AppError::Conflict.into());
    }
    frozen(db, &expected).map(Some)
}

/// The private manifest frozen when the batch started.
pub(super) fn frozen(db: &Connection, batch: &str) -> Result<FrozenManifest, StoreError> {
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
pub(super) fn assessment_errors(
    manifest: &FrozenManifest,
    declared: &[Assessment],
) -> Vec<HandoffFieldError> {
    let expected = tracked(manifest);
    let mut assessed = BTreeSet::new();
    let mut errors = Vec::new();
    for (index, assessment) in declared.iter().enumerate() {
        let discussion = assessment.discussion_id.as_str();
        let path = format!("/handoff/assessments/{index}");
        let Some(ids) = expected.get(discussion) else {
            errors.push(HandoffFieldError::new(
                format!("{path}/discussionId"),
                format!("{discussion} is not a discussion of the selected-thread batch."),
            ));
            continue;
        };
        if !assessed.insert(discussion) {
            errors.push(HandoffFieldError::new(
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
            errors.push(HandoffFieldError::new(
                format!("{path}/findingIds"),
                format!("Include {named} for discussion {discussion}."),
            ));
        }
    }
    for discussion in expected.keys() {
        if !assessed.contains(discussion) {
            errors.push(HandoffFieldError::new(
                "/handoff/assessments",
                format!("Add an assessment for discussion {discussion}."),
            ));
        }
    }
    errors
}

pub(super) fn complete(
    tx: &mut WriteTx<'_>,
    work: &ReportWork,
    manifest: &FrozenManifest,
    accepted_receipt: &str,
    time: i64,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    let review = work.call.lead.review_id().as_str();
    let run = work.call.lead.run_id().as_str();
    let kind = match work.declaration {
        Declaration::Final { .. } => "final",
        Declaration::Unchanged { .. } => "unchanged",
        Declaration::NeedsInput { .. } => return Err(AppError::Internal.into()),
    };
    db.execute(
        "INSERT INTO followup_results(review_id,batch_id,run_id,receipt_id,kind,checked_sha,completed_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
         params![review,manifest.batch_id,run,accepted_receipt,kind,manifest.source_sha,time],
    )?;
    for assessment in work.declaration.assessments() {
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
    super::reconciliation::accept(db, review, &manifest.batch_id)?;
    tx.emit(&[ResourceKey::Followup(Some(
        work.call.lead.review_id().clone(),
    ))])?;
    Ok(())
}

/// Each outcome's discussion links name batch discussions that track its finding, and each
/// linked discussion's assessment covers the finding. An outcome without links is linked to
/// every batch discussion that tracks its finding. A discussion's assessment must then agree
/// with the findings this handoff retires; see [`conclusion_errors`].
pub(super) fn edit_errors(
    db: &Connection,
    run: &str,
    manifest: &FrozenManifest,
    edits: &ReportEdits,
    assessments: &[Assessment],
) -> Result<Vec<HandoffFieldError>, StoreError> {
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
    for (index, edit) in edits.findings.iter().enumerate() {
        let finding = edit.finding_id.as_str();
        let known: bool = existing
            .query_row(params![run, manifest.report_receipt_id, finding], |row| {
                row.get(0)
            })?;
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
            errors.push(HandoffFieldError::new(
                format!("/handoff/findingChanges/{index}/discussionIds"),
                message,
            ));
        }
    }
    errors.extend(conclusion_errors(&tracked, edits, assessments));
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

/// A discussion linked to a retiring outcome for a finding it tracks has an assessment that
/// agrees: retiring when this handoff retires every finding the discussion tracks, and
/// retiring or `partly_addressed` when it retires only some of them.
fn conclusion_errors(
    tracked: &BTreeMap<&str, BTreeSet<&str>>,
    edits: &ReportEdits,
    assessments: &[Assessment],
) -> Vec<HandoffFieldError> {
    let retiring = || {
        edits
            .findings
            .iter()
            .filter(|edit| edit.conclusion.retires_finding())
    };
    let retired: BTreeSet<&str> = retiring().map(|edit| edit.finding_id.as_str()).collect();
    let mut errors = Vec::new();
    for (index, assessment) in assessments.iter().enumerate() {
        let discussion = assessment.discussion_id.as_str();
        let Some(ids) = tracked.get(discussion) else {
            continue;
        };
        if !retiring().any(|edit| {
            ids.contains(edit.finding_id.as_str()) && linked(tracked, edit).contains(discussion)
        }) {
            continue;
        }
        let conclusion = assessment.conclusion;
        let message = if ids.is_subset(&retired) {
            if conclusion.retires_finding() {
                continue;
            }
            format!(
                "This handoff retires every finding discussion {discussion} tracks, so use fixed, clarified_withdrawn or superseded."
            )
        } else {
            if conclusion.retires_finding() || conclusion == FindingConclusion::PartlyAddressed {
                continue;
            }
            format!(
                "This handoff retires some of the findings discussion {discussion} tracks, so use partly_addressed, fixed, clarified_withdrawn or superseded."
            )
        };
        errors.push(HandoffFieldError::new(
            format!("/handoff/assessments/{index}/conclusion"),
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
        let fixed = |discussion_ids: Vec<String>| ReportEdits {
            findings: vec![FindingEdit {
                finding_id: "F-01".into(),
                conclusion: FindingConclusion::Fixed,
                reason: "The retry is bounded.".into(),
                checked_sha: None,
                discussion_ids,
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
        for edits in [fixed(Vec::new()), fixed(vec!["D1".into()])] {
            let partly = assessment(FindingConclusion::PartlyAddressed);
            assert!(conclusion_errors(&tracked, &edits, &partly).is_empty());
            let present = assessment(FindingConclusion::StillPresent);
            let errors = conclusion_errors(&tracked, &edits, &present);
            assert_eq!(
                errors
                    .iter()
                    .map(|error| error.path.as_str())
                    .collect::<Vec<_>>(),
                ["/handoff/assessments/0/conclusion"]
            );
        }
        let mut both = fixed(Vec::new());
        both.findings.push(FindingEdit {
            finding_id: "F-02".into(),
            ..both.findings[0].clone()
        });
        let partly = assessment(FindingConclusion::PartlyAddressed);
        assert_eq!(conclusion_errors(&tracked, &both, &partly).len(), 1);
    }
}
