use std::collections::{BTreeMap, BTreeSet};

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::reports::{Assessment, Declaration, ReportWork};
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
    Ok(db.query_row(
        "SELECT b.batch_id FROM followup_batches b JOIN workflow_dispatch w ON w.batch_id=b.batch_id
         JOIN messages m ON m.message_id=w.delivery_id
         WHERE b.review_id=?1 AND b.run_id=?2 AND b.lead_session_key=?3 AND b.state='executing'
           AND b.authority_verified=1 AND b.native_turn_id=?4 AND m.native_turn_id=?4 AND m.state='started'",
        params![call.lead.review_id().as_str(),call.lead.run_id().as_str(),
            call.lead.session_key().as_str(),call.turn_id.as_str()],|row|row.get(0),
    ).optional()?)
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
    let source: String = db.query_row(
        "SELECT private_manifest FROM followup_batches WHERE batch_id=?1",
        [expected],
        |row| row.get(0),
    )?;
    let value: FrozenManifest = serde_json::from_str(&source).map_err(|_| AppError::Internal)?;
    Ok(Some(value))
}

pub(super) fn assessments_match(manifest: &FrozenManifest, declared: &[Assessment]) -> bool {
    let mut expected = BTreeMap::<&str, BTreeSet<&str>>::new();
    for item in &manifest.observations {
        expected
            .entry(&item.discussion_id)
            .or_default()
            .extend(item.finding_ids.iter().map(String::as_str));
    }
    expected.len() == declared.len()
        && declared.iter().all(|a| {
            expected.get(a.discussion_id.as_str()).is_some_and(|ids| {
                ids.iter()
                    .all(|id| a.finding_ids.iter().any(|actual| actual == id))
            })
        })
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
    tx.emit(&[ResourceKey::Followup(Some(
        work.call.lead.review_id().clone(),
    ))])?;
    Ok(())
}

type Range = (u64, u64, String);
type Findings = BTreeMap<String, Option<Range>>;

pub(super) struct SourceCheck {
    previous: Option<(String, Findings)>,
    candidate: String,
    findings: Findings,
    declared: BTreeMap<String, bool>,
}

pub(super) fn finding_ranges(index: &[reviewq_app::reports::Descriptor]) -> Findings {
    use reviewq_app::reports::Descriptor;
    index
        .iter()
        .filter_map(|item| match item {
            Descriptor::Finding { id, source, .. } => Some((
                id.clone(),
                source
                    .as_ref()
                    .map(|range| (range.offset, range.length, range.hash.to_string())),
            )),
            _ => None,
        })
        .collect()
}

pub(super) fn source_check(
    db: &Connection,
    work: &ReportWork,
    candidate: Option<Findings>,
) -> Result<Option<SourceCheck>, StoreError> {
    let Some(candidate_findings) = candidate else {
        return Ok(None);
    };
    let Some(scope) = manifest(db, work)? else {
        return Ok(None);
    };
    let Some(artifact) = super::ensure_claim(db, work)? else {
        return Ok(None);
    };
    let Some(candidate) = read_semantic_body(db, &artifact)? else {
        return Ok(None);
    };
    let run = work.call.lead.run_id().as_str();
    let mut declared = BTreeMap::<String, bool>::new();
    for assessment in work.declaration.assessments() {
        use reviewq_app::reports::FindingConclusion;
        let permits_removal = matches!(
            assessment.conclusion,
            FindingConclusion::Fixed
                | FindingConclusion::ClarifiedWithdrawn
                | FindingConclusion::Superseded
        );
        for id in &assessment.finding_ids {
            declared
                .entry(id.clone())
                .and_modify(|all| *all &= permits_removal)
                .or_insert(permits_removal);
        }
    }
    let previous =
        if let Some(receipt) = scope.report_receipt_id {
            let artifact=db.query_row(
            "SELECT a.artifact_id FROM report_versions v JOIN artifacts a USING(artifact_id)
             WHERE run_id=?1 AND receipt_id=?2 AND a.sealed=1 AND a.complete=1
               AND v.semantic_index='ready' AND a.sha256=?3",
            params![run,receipt,scope.report_hash],|row|row.get::<_,String>(0),
        ).optional()?;
            let Some(artifact) = artifact else {
                return Ok(None);
            };
            let Some(previous) = read_semantic_body(db, &artifact)? else {
                return Ok(None);
            };
            let mut statement = db.prepare_cached(
                "SELECT finding_id,scalar_offset,scalar_length,evidence_sha256 FROM report_findings
             WHERE run_id=?1 AND receipt_id=?2 ORDER BY finding_id",
            )?;
            let mut rows = statement.query(params![run, receipt])?;
            let mut findings = Findings::new();
            while let Some(row) = rows.next()? {
                let id = row.get(0)?;
                let start: Option<i64> = row.get(1)?;
                let range = if let Some(start) = start {
                    Some((
                        u64::try_from(start).map_err(|_| AppError::Internal)?,
                        u64::try_from(row.get::<_, i64>(2)?).map_err(|_| AppError::Internal)?,
                        row.get(3)?,
                    ))
                } else {
                    None
                };
                findings.insert(id, range);
            }
            Some((previous, findings))
        } else {
            None
        };
    Ok(Some(SourceCheck {
        previous,
        candidate,
        findings: candidate_findings,
        declared,
    }))
}

fn read_semantic_body(db: &Connection, artifact: &str) -> Result<Option<String>, StoreError> {
    const LIMIT: i64 = 4 * 1024 * 1024;
    let length: i64 = db.query_row(
        "SELECT byte_length FROM artifacts WHERE artifact_id=?1",
        [artifact],
        |row| row.get(0),
    )?;
    if length > LIMIT {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    let mut statement =
        db.prepare_cached("SELECT raw FROM artifact_chunks WHERE artifact_id=?1 ORDER BY ordinal")?;
    let mut rows = statement.query([artifact])?;
    while let Some(row) = rows.next()? {
        let chunk = row.get_ref(0)?.as_blob().map_err(|_| AppError::Internal)?;
        if bytes.len().saturating_add(chunk.len()) > length as usize {
            return Err(AppError::Internal.into());
        }
        bytes.extend_from_slice(chunk);
    }
    if bytes.len() != length as usize {
        return Err(AppError::Internal.into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| AppError::Internal.into())
}

pub(super) fn source_preserved(input: Option<SourceCheck>) -> bool {
    let Some(input) = input else { return false };
    let Some((old, older)) = input.previous else {
        return input
            .findings
            .keys()
            .all(|id| input.declared.contains_key(id));
    };
    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    for (id, range) in &older {
        if let Some(may_remove) = input.declared.get(id) {
            let Some((start, length, _)) = range else {
                return false;
            };
            old_ranges.push((*start, *length));
            if !may_remove && !input.findings.contains_key(id) {
                return false;
            }
        } else if input
            .findings
            .get(id)
            .and_then(|value| value.as_ref().map(|value| &value.2))
            != range.as_ref().map(|value| &value.2)
            || !input.findings.contains_key(id)
        {
            return false;
        }
    }
    for (id, range) in &input.findings {
        if input.declared.contains_key(id) {
            let Some((start, length, _)) = range else {
                return false;
            };
            new_ranges.push((*start, *length));
        } else if !older.contains_key(id) {
            return false;
        }
    }
    match (
        unaffected(&old, old_ranges),
        unaffected(&input.candidate, new_ranges),
    ) {
        (Some(old), Some(new)) => old == new,
        _ => false,
    }
}

fn unaffected(source: &str, mut ranges: Vec<(u64, u64)>) -> Option<Vec<u8>> {
    ranges.sort_unstable();
    let mut output = Vec::new();
    let mut cursor = source
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(source.len()))
        .enumerate()
        .peekable();
    let mut previous = 0;
    let mut end = 0_u64;
    for (start, length) in ranges {
        if start < end || length == 0 {
            return None;
        }
        let start = usize::try_from(start).ok()?;
        let right = usize::try_from(length).ok()?.checked_add(start)?;
        while cursor.peek().is_some_and(|(index, _)| *index < start) {
            cursor.next();
        }
        let &(actual, left) = cursor.peek()?;
        if actual != start {
            return None;
        }
        while cursor.peek().is_some_and(|(index, _)| *index < right) {
            cursor.next();
        }
        let &(actual, after) = cursor.peek()?;
        if actual != right {
            return None;
        }
        output.extend_from_slice(&source.as_bytes()[previous..left]);
        previous = after;
        end = right as u64;
    }
    output.extend_from_slice(&source.as_bytes()[previous..]);
    Some(output)
}

pub(super) fn write_changes(
    db: &Connection,
    run: &str,
    receipt: &str,
    manifest: &FrozenManifest,
    assessments: &[Assessment],
) -> Result<(), StoreError> {
    let mut changes = BTreeMap::new();
    for assessment in assessments {
        for finding in &assessment.finding_ids {
            changes.entry(finding).or_insert(assessment);
        }
    }
    for (finding, assessment) in changes {
        db.prepare_cached(
            "INSERT INTO report_changes(run_id,receipt_id,finding_id,conclusion,reason,checked_sha)
             VALUES(?1,?2,?3,?4,?5,?6)",
        )?
        .execute(params![
            run,
            receipt,
            finding,
            assessment.conclusion.to_string(),
            assessment.reason,
            manifest.source_sha
        ])?;
    }
    for assessment in assessments {
        for finding in &assessment.finding_ids {
            db.prepare_cached(
                "INSERT INTO report_change_discussions(run_id,receipt_id,finding_id,discussion_id)
                 VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",
            )?
            .execute(params![run, receipt, finding, assessment.discussion_id])?;
        }
    }
    Ok(())
}
