use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::reports::{
    CapturedArtifact, Declaration, Descriptor, ReportReference, ReportWork, RevisionEvidence,
    Source as ReportSource, Verdict,
};
use reviewq_domain::identity::NativeInputId;
use rusqlite::{Connection, OptionalExtension, params};

use super::{db_int, ensure_claim};
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::notifications::insert_notification;
use crate::queue::identity_for_review;
use crate::workspace::{advance_queue, configured};

pub(super) fn complete(
    tx: &mut WriteTx<'_>,
    work: ReportWork,
    result: NativeInputId,
    artifact: Option<CapturedArtifact>,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    let stage = ensure_claim(db, &work)?;
    let run = work.call.lead.run_id().as_str();
    let receipt = work.acknowledgement.receipt_id.as_str();
    let time = now_ms()?;
    if !configured(db, &identity_for_review(db, work.call.lead.review_id())?)? {
        return reject(
            tx,
            &work,
            artifact.as_ref(),
            "report_workspace_changed",
            Some(&result),
        );
    }
    let current = head(db, run)?;
    let base = db.query_row(
        "SELECT base_receipt_id, disposition_version FROM report_handoffs WHERE receipt_id=?1",
        [receipt],
        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let newer: bool = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM report_handoffs
             WHERE run_id=?1 AND rowid>(SELECT rowid FROM report_handoffs WHERE receipt_id=?2)
               AND state<>'failed')",
        params![run, receipt],
        |row| row.get(0),
    )?;
    if newer || current.receipt.as_ref() != base.0.as_ref() || current.disposition_version != base.1
    {
        return reject(
            tx,
            &work,
            artifact.as_ref(),
            "report_base_changed",
            Some(&result),
        );
    }
    let notification = match &work.declaration {
        Declaration::Final {
            revision,
            verdict,
            amends,
            ..
        } => {
            let captured = artifact.as_ref().ok_or(AppError::Internal)?;
            let artifact_id = stage.as_deref().ok_or(AppError::Conflict)?;
            if let Some(reference) = amends {
                if !current.matches(reference)
                    || current.hash.as_deref() != Some(captured.hash.as_str())
                    || current.verdict.as_deref() != Some(verdict.to_string().as_str())
                    || !current.matches_revision(revision)
                {
                    return reject(
                        tx,
                        &work,
                        Some(captured),
                        "report_amendment_continuity_unproved",
                        Some(&result),
                    );
                }
            }
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
                captured,
                time,
            )?;
            amends
                .is_none()
                .then_some((Kind::ReportReady, "Report ready"))
        }
        Declaration::Unchanged { accepted, source } => {
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
            if !current.matches(accepted) || !original {
                return reject(
                    tx,
                    &work,
                    None,
                    "report_unchanged_evidence_mismatch",
                    Some(&result),
                );
            }
            None
        }
        Declaration::NeedsInput { .. } => Some((Kind::AgentQuestion, "Review needs input")),
    };
    db.execute(
        "UPDATE report_handoffs SET state='recorded', result_entry_id=?2,
            updated_at_ms=max(updated_at_ms, ?3) WHERE receipt_id=?1",
        params![receipt, result.as_str(), time],
    )?;
    emit(tx, &work, time, notification)
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
    emit(
        tx,
        work,
        time,
        Some((Kind::ReportFailed, "Report could not be accepted")),
    )
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
                v.revision_reason, v.full_mr_reviewed_sha, v.verdict, v.coverage, c.source_kind, c.source_path
         FROM report_heads h JOIN report_versions v USING(run_id, receipt_id)
         JOIN artifacts a USING(artifact_id)
         LEFT JOIN report_handoffs c ON c.run_id=v.run_id AND c.receipt_id=v.handoff_receipt_id
         WHERE h.run_id=?1",
        [run], |row| Ok(Head {
            receipt: row.get(0)?, disposition_version: row.get(1)?, hash: row.get(2)?,
            attention: row.get(3)?, reviewed_sha: row.get(4)?, revision_reason: row.get(5)?,
            full_sha: row.get(6)?, verdict: row.get(7)?, coverage: row.get(8)?, source_kind: row.get(9)?, source_path: row.get(10)?,
        }),
    ).optional()?.unwrap_or(Head {
        receipt: None, disposition_version: 0, hash: None, attention: None,
        reviewed_sha: None, revision_reason: None, full_sha: None, verdict: None, coverage: None, source_kind: None, source_path: None,
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
    captured: &CapturedArtifact,
    time: i64,
) -> Result<(), StoreError> {
    let (reviewed, reason) = match revision {
        RevisionEvidence::Exact(sha) => (Some(sha.as_str()), None),
        RevisionEvidence::Unknown(reason) => (None, Some(reason.as_str())),
    };
    let known_full = if amendment {
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
    } else {
        "full_mr"
    };
    let count = captured.index.as_ref().map(|items| {
        items
            .iter()
            .filter(|item| matches!(item, Descriptor::Finding { .. }))
            .count() as i64
    });
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
          starting_disposition_version)
         VALUES(?1,?2,?3,?2,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![run, receipt, artifact, previous.receipt, attention, reviewed, reason, known_full,
            coverage, verdict.to_string(), semantic, count, count.map(|_| 0_i64), time,
            previous.disposition_version],
    )?;
    if let Some(items) = &captured.index {
        write_index(db, run, receipt, items)?;
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
            Descriptor::Section { title, source } => {
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
            },
        )? {
            keys.push(ResourceKey::Notification(Some(id)));
        }
    }
    advance_queue(&tx.sql)?;
    tx.emit(&keys)?;
    Ok(())
}
