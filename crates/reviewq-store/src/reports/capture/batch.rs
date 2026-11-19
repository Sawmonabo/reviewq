use std::collections::{BTreeMap, BTreeSet};

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::reports::{Assessment, Declaration, ReportEdits, ReportWork};
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

pub(super) fn read_semantic_body(
    db: &Connection,
    artifact: &str,
) -> Result<Option<String>, StoreError> {
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

pub(super) fn unaffected(source: &str, mut ranges: Vec<(u64, u64)>) -> Option<Vec<u8>> {
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

pub(super) fn edits_match(
    db: &Connection,
    run: &str,
    manifest: &FrozenManifest,
    edits: &ReportEdits,
    assessments: &[Assessment],
) -> Result<bool, StoreError> {
    let originals: BTreeSet<&str> = manifest
        .observations
        .iter()
        .map(|member| member.discussion_id.as_str())
        .collect();
    if edits.sections.iter().any(|section| {
        section
            .discussion_ids
            .iter()
            .any(|id| !originals.contains(id.as_str()))
    }) {
        return Ok(false);
    }
    let mut existing = db.prepare_cached(
        "SELECT EXISTS(SELECT 1 FROM report_findings WHERE run_id=?1 AND receipt_id=?2 AND finding_id=?3)",
    )?;
    for edit in &edits.findings {
        let known: bool = existing.query_row(
            params![run, manifest.report_receipt_id, edit.finding_id],
            |row| row.get(0),
        )?;
        if edit.discussion_ids.iter().any(|id| {
            !originals.contains(id.as_str())
                || (known
                    && !manifest.observations.iter().any(|observation| {
                        &observation.discussion_id == id
                            && observation.finding_ids.contains(&edit.finding_id)
                    }))
                || !assessments.iter().any(|assessment| {
                    &assessment.discussion_id == id
                        && assessment.finding_ids.contains(&edit.finding_id)
                        && (!edit.conclusion.retires_finding()
                            || assessment.conclusion.retires_finding())
                })
        }) {
            return Ok(false);
        }
    }
    Ok(true)
}
