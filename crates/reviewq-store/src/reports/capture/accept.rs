use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::reports::{
    CapturedArtifact, Declaration, Descriptor, ReportEdits, ReportReference, ReportWork,
    RevisionEvidence, Source as ReportSource, StructuredReport, Verdict,
};
use reviewq_app::workflow::ReportFailureAddress;
use reviewq_domain::identity::NativeInputId;
use rusqlite::{Connection, OptionalExtension, params};

use super::{authority, batch, db_int, edits, ensure_claim, failures, structured};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::notifications::insert_notification;
use crate::queue::identity_for_review;
use crate::workspace::{advance_queue, settings_revision};

pub(super) fn complete(
    tx: &mut WriteTx<'_>,
    work: ReportWork,
    result: NativeInputId,
    artifact: Option<CapturedArtifact>,
    resolved: Vec<ReportFailureAddress>,
) -> Result<bool, StoreError> {
    let db = &tx.sql;
    let stage = ensure_claim(db, &work)?;
    let run = work.call.lead.run_id().as_str();
    let receipt = work.acknowledgement.receipt_id.as_str();
    let time = now_ms()?;
    let identity = identity_for_review(db, work.call.lead.review_id())?;
    let Some(authority) = authority::for_project(db, &identity)? else {
        return reject(
            tx,
            &work,
            artifact.as_ref(),
            "report_workspace_changed",
            Some(&result),
        )
        .map(|()| false);
    };
    if let Some(hold) =
        crate::hosts::unresolved_workflow_input(db, &work.call.lead, &work.call.turn_id)?
    {
        if hold == AppError::Busy {
            db.execute(
                "UPDATE report_handoffs SET due_at_ms=?2 WHERE receipt_id=?1",
                params![receipt, time.saturating_add(super::RECHECK_MS)],
            )?;
            return Ok(false);
        }
        return reject(
            tx,
            &work,
            artifact.as_ref(),
            "workflow_turn_start_unproved",
            Some(&result),
        )
        .map(|()| false);
    }
    let current = head(db, run)?;
    let (base_receipt, legacy_settings, saved_authority, grant_current) = db.query_row(
        "SELECT base_receipt_id, settings_revision, json_extract(edits, '$.authority'),
            json_extract(edits, '$.grantId') IS NULL OR EXISTS (
                SELECT 1 FROM repo_grants g WHERE g.grant_id=json_extract(h.edits, '$.grantId')
                AND g.epoch=json_extract(h.edits, '$.grantEpoch') AND g.active=1
                AND g.origin=?2 AND g.project_id=?3)
         FROM report_handoffs h WHERE receipt_id=?1",
        params![
            receipt,
            identity.origin().as_str(),
            identity.project_id().as_sqlite_integer()
        ],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, bool>(3)?,
            ))
        },
    )?;
    let newer = newer_handoff(db, receipt)?;
    // Receipts staged before scoped authority was recorded retain their original guard.
    let authority_changed = match saved_authority {
        Some(saved) => saved != authority.0.as_str(),
        None => legacy_settings != settings_revision(db)?.as_sqlite_integer(),
    };
    if newer || current.receipt != base_receipt || !grant_current || authority_changed {
        return reject(
            tx,
            &work,
            artifact.as_ref(),
            "report_base_changed",
            Some(&result),
        )
        .map(|()| false);
    }
    let scoped = match batch::manifest(db, &work).map_err(StoreError::app) {
        Ok(value) => value,
        Err(AppError::Conflict) => {
            return reject(
                tx,
                &work,
                artifact.as_ref(),
                "followup_binding_changed",
                Some(&result),
            )
            .map(|()| false);
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(scope) = &scoped
        && (scope.report_receipt_id != current.receipt
            || scope.report_hash != current.hash
            || ((!matches!(work.declaration, Declaration::NeedsInput { .. })
                || !work.declaration.assessments().is_empty())
                && !batch::assessments_match(scope, work.declaration.assessments())))
    {
        return reject(
            tx,
            &work,
            artifact.as_ref(),
            "followup_scope_changed",
            Some(&result),
        )
        .map(|()| false);
    }
    let notification = match &work.declaration {
        Declaration::Final {
            revision,
            verdict,
            amends,
            edits,
            report,
            ..
        } => {
            let captured = artifact.as_ref().ok_or(AppError::Internal)?;
            let artifact_id = stage.as_deref().ok_or(AppError::Conflict)?;
            if let Some(scope) = &scoped
                && (!batch::edits_match(db, run, scope, edits, work.declaration.assessments())?
                    || !matches!(revision, RevisionEvidence::Exact(sha) if sha.as_str() == scope.source_sha))
            {
                return reject(
                    tx,
                    &work,
                    Some(captured),
                    "followup_scope_changed",
                    Some(&result),
                )
                .map(|()| false);
            }
            if let Some(reference) = amends
                && (!current.complete
                    || !current.semantic_ready
                    || !current.matches(reference)
                    || current
                        .original_hash
                        .as_ref()
                        .is_some_and(|hash| Some(hash) != current.hash.as_ref())
                    || current.hash.as_deref() != Some(captured.hash.as_str())
                    || current.verdict.as_deref() != Some(verdict.to_string().as_str())
                    || !current.matches_revision(revision))
            {
                return reject(
                    tx,
                    &work,
                    Some(captured),
                    "report_amendment_continuity_unproved",
                    Some(&result),
                )
                .map(|()| false);
            }
            let projection = if let Some(report) = report {
                let projection = structured::prepare(
                    db,
                    run,
                    current.receipt.as_deref(),
                    report,
                    revision,
                    edits,
                )?;
                if !projection.fits(db, receipt)? {
                    return reject(
                        tx,
                        &work,
                        Some(captured),
                        "report_structured_payload_too_large",
                        Some(&result),
                    )
                    .map(|()| false);
                }
                Some((projection, report))
            } else {
                None
            };
            seal(db, artifact_id, captured)?;
            promote(
                db,
                run,
                receipt,
                artifact_id,
                &current,
                revision,
                *verdict,
                amends.is_some(),
                edits,
                captured,
                projection
                    .as_ref()
                    .map(|(projection, report)| (projection, report.as_ref())),
                time,
                scoped
                    .as_ref()
                    .map(|manifest| (manifest, work.declaration.assessments())),
            )?;
            amends
                .is_none()
                .then_some((Kind::ReportReady, "Report ready"))
        }
        Declaration::Unchanged {
            accepted, source, ..
        } => {
            let original = match source {
                ReportSource::Response => current.source_kind.as_deref() == Some("response"),
                ReportSource::File { path, .. } => {
                    use std::os::unix::ffi::OsStrExt;
                    current.source_kind.as_deref() == Some("file")
                        && current.source_path.as_deref()
                            == Some(path.as_path().as_os_str().as_bytes())
                        && artifact
                            .as_ref()
                            .is_some_and(|captured| accepted.hash == captured.hash)
                }
            };
            if !current.complete
                || current
                    .original_hash
                    .as_ref()
                    .is_some_and(|hash| Some(hash) != current.hash.as_ref())
                || !current.matches(accepted)
                || !original
            {
                return reject(
                    tx,
                    &work,
                    artifact.as_ref(),
                    "report_unchanged_evidence_mismatch",
                    Some(&result),
                )
                .map(|()| false);
            }
            if let Some(staged) = stage {
                db.execute(
                    "UPDATE report_handoffs SET artifact_id=NULL WHERE receipt_id=?1",
                    [receipt],
                )?;
                db.execute(
                    "DELETE FROM artifacts WHERE artifact_id=?1 AND sealed=0",
                    [staged],
                )?;
            }
            scoped
                .is_some()
                .then_some((Kind::FollowupReady, "Review follow-up ready"))
        }
        Declaration::NeedsInput { .. } => Some((Kind::AgentQuestion, "Review needs input")),
    };
    db.execute(
        "UPDATE report_handoffs SET state='recorded', result_entry_id=?2,
            updated_at_ms=max(updated_at_ms, ?3) WHERE receipt_id=?1",
        params![receipt, result.as_str(), time],
    )?;
    if let Declaration::Final {
        revision: RevisionEvidence::Exact(sha),
        amends: None,
        edits,
        ..
    } = &work.declaration
        && edits.correction.is_none()
    {
        failures::resolve(db, &work, sha, resolved)?;
    }
    if let Some(manifest) = &scoped {
        match &work.declaration {
            Declaration::Final { .. } => batch::complete(tx, &work, manifest, receipt, time)?,
            Declaration::Unchanged { accepted, .. } => {
                batch::complete(tx, &work, manifest, accepted.receipt_id.as_str(), time)?
            }
            Declaration::NeedsInput { .. } => {
                tx.sql.execute(
                    "UPDATE followup_batches SET hold_reason='followup_native_input_required' WHERE batch_id=?1
                     AND state='executing' AND authority_verified=1", [&manifest.batch_id],
                )?;
                tx.emit(&[ResourceKey::Followup(Some(
                    work.call.lead.review_id().clone(),
                ))])?;
            }
        }
    }
    if !matches!(work.declaration, Declaration::NeedsInput { .. }) {
        crate::hosts::recovery::complete_turn(tx, &work.call.lead, &work.call.turn_id)?;
    }
    emit(tx, &work, time, notification)?;
    Ok(matches!(work.declaration, Declaration::Final { .. }))
}

fn newer_handoff(db: &Connection, staged_receipt: &str) -> Result<bool, StoreError> {
    // A missing admission count preserves the guard for pre-upgrade stages.
    // Across generations, pending calls and changes in the recorded count
    // indicate competing submissions without a comparable native order.
    Ok(db.query_row(
        "WITH competing AS (
            SELECT other.state, reviewq_report_order(other.source_generation,other.source_position,
                       staged.source_generation,staged.source_position) AS native_order
            FROM report_handoffs staged JOIN report_handoffs other USING(run_id)
            WHERE staged.receipt_id=?1 AND other.receipt_id<>staged.receipt_id
         )
         SELECT count(*) FILTER (WHERE coalesce(native_order>0,state='pending_completion'))>0
            OR count(*) FILTER (WHERE native_order IS NULL AND state='recorded')<>coalesce(
                (SELECT json_extract(edits,'$.incomparableRecorded') FROM report_handoffs WHERE receipt_id=?1),0)
         FROM competing",
        [staged_receipt],
        |row| row.get(0),
    )?)
}

pub(super) fn reject(
    tx: &mut WriteTx<'_>,
    work: &ReportWork,
    artifact: Option<&CapturedArtifact>,
    reason: &'static str,
    native_result: Option<&NativeInputId>,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    let candidate = ensure_claim(db, work)?;
    let time = now_ms()?;
    if let (Some(artifact_id), Some(captured)) = (candidate.as_deref(), artifact) {
        seal(db, artifact_id, captured)?;
    } else if let Some(artifact_id) = candidate {
        db.execute(
            "UPDATE report_handoffs SET artifact_id=NULL WHERE receipt_id=?1",
            [work.acknowledgement.receipt_id.as_str()],
        )?;
        db.execute(
            "DELETE FROM artifacts WHERE artifact_id=?1 AND sealed=0",
            [artifact_id],
        )?;
    }
    db.execute(
        "UPDATE report_handoffs SET state='failed', reason=?2, updated_at_ms=max(updated_at_ms, ?3),
            result_entry_id=coalesce(?4,result_entry_id) WHERE receipt_id=?1",
        params![work.acknowledgement.receipt_id.as_str(), reason, time, native_result.map(|id| id.as_str())],
    )?;
    let held = db.execute(
        "UPDATE followup_batches SET hold_reason='followup_reconciliation_pending' WHERE state='executing'
         AND batch_id=(SELECT batch_id FROM report_handoffs WHERE receipt_id=?1)
         AND coalesce(continuation_turn_id,native_turn_id)=?2",
        params![work.acknowledgement.receipt_id.as_str(),work.call.turn_id.as_str()],
    )?;
    if held == 1 {
        tx.emit(&[ResourceKey::Followup(Some(
            work.call.lead.review_id().clone(),
        ))])?;
    }
    if matches!(work.declaration, Declaration::Final { .. }) {
        if let Some(notification) = failures::note(
            &tx.sql,
            &work.call,
            work.acknowledgement.receipt_id.to_string(),
            reason,
            "report_delivery_failure",
            time,
        )? {
            tx.emit(&[ResourceKey::Notification(Some(notification))])?;
        }
        emit(tx, work, time, None)
    } else {
        emit(
            tx,
            work,
            time,
            Some((Kind::ReportFailed, "Report could not be accepted")),
        )
    }
}

struct Head {
    receipt: Option<String>,
    disposition_version: i64,
    hash: Option<String>,
    attention: Option<String>,
    reviewed_sha: Option<String>,
    revision_reason: Option<String>,
    full_sha: Option<String>,
    verdict: Option<String>,
    coverage: Option<String>,
    source_kind: Option<String>,
    source_path: Option<Vec<u8>>,
    complete: bool,
    semantic_ready: bool,
    change_count: Option<i64>,
    finding_count: Option<i64>,
    original_hash: Option<String>,
    coverage_source: Option<String>,
    coverage_target: Option<String>,
}

impl Head {
    fn matches(&self, reference: &ReportReference) -> bool {
        self.receipt.as_deref() == Some(reference.receipt_id.as_str())
            && self.hash.as_deref() == Some(reference.hash.as_str())
    }

    fn matches_revision(&self, evidence: &RevisionEvidence) -> bool {
        match evidence {
            RevisionEvidence::Exact(sha) => self.reviewed_sha.as_deref() == Some(sha.as_str()),
            RevisionEvidence::Unknown(reason) => self.revision_reason.as_deref() == Some(reason),
        }
    }
}

fn head(db: &Connection, run: &str) -> Result<Head, StoreError> {
    Ok(db.query_row(
        "SELECT h.receipt_id, h.disposition_version, a.sha256, v.attention_receipt_id, v.reviewed_sha,
                v.revision_reason, v.full_mr_reviewed_sha, v.verdict, v.coverage, c.source_kind, c.source_path, a.complete, a.original_sha256, v.coverage_source_sha, v.coverage_target_sha, v.semantic_index='ready', v.change_count, v.finding_count
         FROM report_heads h JOIN report_versions v USING(run_id, receipt_id)
         JOIN artifacts a USING(artifact_id)
         LEFT JOIN report_handoffs c ON c.run_id=v.run_id AND c.receipt_id=v.handoff_receipt_id
         WHERE h.run_id=?1",
        [run], |row| Ok(Head {
            receipt: row.get(0)?, disposition_version: row.get(1)?, hash: row.get(2)?,
            attention: row.get(3)?, reviewed_sha: row.get(4)?, revision_reason: row.get(5)?,
            full_sha: row.get(6)?, verdict: row.get(7)?, coverage: row.get(8)?, source_kind: row.get(9)?, source_path: row.get(10)?, complete: row.get(11)?, original_hash: row.get(12)?, coverage_source: row.get(13)?, coverage_target: row.get(14)?, semantic_ready: row.get(15)?, change_count: row.get(16)?, finding_count: row.get(17)?,
        }),
    ).optional()?.unwrap_or(Head {
        receipt: None, disposition_version: 0, hash: None, attention: None,
        reviewed_sha: None, revision_reason: None, full_sha: None, verdict: None, coverage: None, source_kind: None, source_path: None, complete: false, semantic_ready: false, change_count: None, finding_count: None, original_hash: None, coverage_source: None, coverage_target: None,
    }))
}

fn seal(db: &Connection, artifact: &str, captured: &CapturedArtifact) -> Result<(), StoreError> {
    let stats = &captured.stats;
    let changed = db.execute(
        "UPDATE artifacts SET sha256=?2, line_count=?3, max_line_bytes=?4, complete=1, sealed=1
         WHERE artifact_id=?1 AND byte_length=?5 AND total_code_points=?6 AND sealed=0",
        params![
            artifact,
            captured.hash.as_str(),
            db_int(stats.line_count)?,
            db_int(stats.max_line_bytes)?,
            db_int(stats.byte_length)?,
            db_int(stats.total_code_points)?
        ],
    )?;
    if changed != 1 {
        return Err(AppError::Conflict.into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn promote(
    db: &Connection,
    run: &str,
    receipt: &str,
    artifact: &str,
    previous: &Head,
    revision: &RevisionEvidence,
    verdict: Verdict,
    amendment: bool,
    edits: &ReportEdits,
    captured: &CapturedArtifact,
    structured: Option<(&structured::Projection, &StructuredReport)>,
    time: i64,
    scoped: Option<(
        &crate::followups::work::FrozenManifest,
        &[reviewq_app::reports::Assessment],
    )>,
) -> Result<(), StoreError> {
    let (reviewed, reason) = match revision {
        RevisionEvidence::Exact(sha) => (Some(sha.as_str()), None),
        RevisionEvidence::Unknown(reason) => (None, Some(reason.as_str())),
    };
    let known_full = if amendment || scoped.is_some() {
        previous.full_sha.as_deref()
    } else {
        reviewed.or(previous.full_sha.as_deref())
    };
    let attention = if amendment {
        previous.attention.as_deref().ok_or(AppError::Internal)?
    } else {
        receipt
    };
    let coverage = if amendment {
        previous.coverage.as_deref().ok_or(AppError::Internal)?
    } else if scoped.is_some() {
        "selected_threads"
    } else {
        "full_mr"
    };
    let changes = if amendment {
        previous.change_count.unwrap_or(0)
    } else {
        edits.findings.len() as i64
    };
    let count = if let Some((projection, ..)) = structured {
        Some(projection.count)
    } else if amendment {
        previous.finding_count
    } else {
        captured.index.as_ref().map(|items| {
            items
                .iter()
                .filter(|item| matches!(item, Descriptor::Finding { .. }))
                .count() as i64
        })
    };
    let semantic = if count.is_some() {
        "ready"
    } else {
        "source_only"
    };
    if let Some(parent) = &previous.receipt {
        db.execute(
            "UPDATE report_versions SET closing_disposition_version=?3
             WHERE run_id=?1 AND receipt_id=?2 AND closing_disposition_version IS NULL",
            params![run, parent, previous.disposition_version],
        )?;
    }
    db.execute(
        "INSERT INTO report_versions(run_id, receipt_id, artifact_id, handoff_receipt_id,
          parent_receipt_id, attention_receipt_id, reviewed_sha, revision_reason, full_mr_reviewed_sha,
          coverage, verdict, semantic_index, finding_count, change_count, captured_at_ms,
          starting_disposition_version, coverage_source_sha, coverage_target_sha)
         VALUES(?1,?2,?3,?2,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![run, receipt, artifact, previous.receipt, attention, reviewed, reason, known_full,
            coverage, verdict.to_string(), semantic, count, count.map(|_| changes), time,
            previous.disposition_version,
            if amendment { previous.coverage_source.as_deref() } else { reviewed },
            if amendment { previous.coverage_target.as_deref() } else { scoped.and_then(|(manifest,_)| manifest.target_sha.as_deref()) }],
    )?;
    if let Some((projection, report)) = structured {
        projection.install(
            db,
            run,
            receipt,
            previous.receipt.as_deref(),
            &report.findings,
        )?;
        if let Some(items) = &captured.index {
            write_index(db, run, receipt, items)?;
        }
    } else if amendment {
        let parent = previous.receipt.as_deref().ok_or(AppError::Internal)?;
        db.execute(
            "INSERT INTO report_findings
             SELECT run_id,?3,finding_id,title,severity,blocking,scalar_offset,scalar_length,evidence_sha256
             FROM report_findings WHERE run_id=?1 AND receipt_id=?2",
            params![run,parent,receipt],
        )?;
        db.execute(
            "INSERT INTO report_sections
             SELECT run_id,?3,scalar_offset,scalar_length,title,evidence_sha256
             FROM report_sections WHERE run_id=?1 AND receipt_id=?2",
            params![run, parent, receipt],
        )?;
    } else if let Some(items) = &captured.index {
        write_index(db, run, receipt, items)?;
    }
    if amendment {
        let parent = previous.receipt.as_deref().ok_or(AppError::Internal)?;
        db.execute(
            "INSERT INTO report_changes
             SELECT run_id,?3,finding_id,conclusion,reason,checked_sha FROM report_changes WHERE run_id=?1 AND receipt_id=?2",
            params![run,parent,receipt],
        )?;
        db.execute(
            "INSERT INTO report_change_discussions
             SELECT run_id,?3,finding_id,discussion_id FROM report_change_discussions WHERE run_id=?1 AND receipt_id=?2",
            params![run,parent,receipt],
        )?;
    } else {
        edits::write_changes(db, run, receipt, &edits.findings)?;
    }
    db.execute(
        "INSERT INTO report_heads(run_id, receipt_id, disposition_version) VALUES(?1, ?2, ?3)
         ON CONFLICT(run_id) DO UPDATE SET receipt_id=excluded.receipt_id,
                       disposition_version=excluded.disposition_version",
        params![run, receipt, previous.disposition_version],
    )?;
    Ok(())
}

fn write_index(
    db: &Connection,
    run: &str,
    receipt: &str,
    items: &[Descriptor],
) -> Result<(), StoreError> {
    let mut finding = db.prepare_cached(
        "INSERT INTO report_findings(run_id, receipt_id, finding_id, title, severity, blocking,
          scalar_offset, scalar_length, evidence_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
    )?;
    let mut section = db.prepare_cached(
        "INSERT INTO report_sections(run_id, receipt_id, title, scalar_offset, scalar_length, evidence_sha256)
         VALUES(?1,?2,?3,?4,?5,?6)",
    )?;
    for entry in items {
        match entry {
            Descriptor::Finding {
                id,
                title,
                severity,
                blocking,
                source: Some(source),
                ..
            } => {
                finding.execute(params![
                    run,
                    receipt,
                    id,
                    title,
                    severity,
                    blocking,
                    db_int(source.offset)?,
                    db_int(source.length)?,
                    source.hash.as_str()
                ])?;
            }
            Descriptor::Section { title, source, .. } => {
                section.execute(params![
                    run,
                    receipt,
                    title,
                    db_int(source.offset)?,
                    db_int(source.length)?,
                    source.hash.as_str()
                ])?;
            }
            _ => return Err(AppError::Internal.into()),
        }
    }
    Ok(())
}

fn emit(
    tx: &mut WriteTx<'_>,
    work: &ReportWork,
    time: i64,
    notification: Option<(Kind, &str)>,
) -> Result<(), StoreError> {
    let target = &work.call.lead;
    let mut keys = vec![
        ResourceKey::Queue,
        ResourceKey::Review(Some(target.review_id().clone())),
        ResourceKey::Run(Some(target.run_id().clone())),
        ResourceKey::Report(Some(target.run_id().clone())),
    ];
    if let Some((kind, label)) = notification {
        let id = work.acknowledgement.receipt_id.to_string();
        if let Some(id) = insert_notification(
            &tx.sql,
            Observation {
                source: Source::Report,
                occurrence_id: id.clone(),
                kind,
                review: target.review_id().clone(),
                run: Some(target.run_id().clone()),
                session: Some(target.session_key().clone()),
                event_ref: Some(id),
                label: label.to_owned(),
                occurred_at_ms: time,
                desktop_episode: None,
            },
        )? {
            keys.push(ResourceKey::Notification(Some(id)));
        }
    }
    advance_queue(&tx.sql)?;
    tx.emit(&keys)?;
    Ok(())
}

#[cfg(test)]
mod tests;
