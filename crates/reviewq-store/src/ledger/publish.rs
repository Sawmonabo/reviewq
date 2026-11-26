//! `commit_publish`: one immediate transaction, conditional on the snapshot's guard, that
//! stores a composed version or a closing call answered with the current version; and the
//! display index after it.

use reviewq_app::access::AppError;
use reviewq_app::events::ResourceKey;
use reviewq_app::followups::ResultKind;
use reviewq_app::ledger::{
    ClosingCommit, CommitKind, FinishReceipt, NewVersion, PublishCommit, PublishOutcome,
    SnapshotGuard,
};
use reviewq_app::notifications::{Kind, Observation, Source};
use reviewq_app::reports::RevisionEvidence;
use reviewq_domain::identity::{ReceiptId, ReportHash, RunId};
use reviewq_domain::ledger::Tool;
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::calls::{self, finish_json};
use super::passes::{self, RUN_TARGET};
use super::sequence;
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::followups::completion;
use crate::followups::work::FrozenManifest;
use crate::hosts::recovery::complete_turn;
use crate::notifications::{insert_notification, publish, withdraw};
use crate::queue::{identity_for_review, parse};
use crate::reports::artifact;
use crate::reports::authority::for_project;
use crate::reports::failures;
use crate::workspace::advance_queue;
use crate::{SqliteStore, StoreError};

pub(super) fn commit(
    tx: &mut WriteTx<'_>,
    commit: PublishCommit,
) -> Result<PublishOutcome, StoreError> {
    let PublishCommit { guard, kind } = commit;
    let db = &tx.sql;
    let run = guard.lead.run_id().as_str();
    // A closing call already stored, by a concurrent request with its call ID: the next
    // snapshot replays its acknowledgement.
    if let CommitKind::Closing(closing) = &kind
        && calls::replay(db, &closing.call, Tool::FinishReview, RUN_TARGET)?.is_some()
    {
        return Ok(PublishOutcome::Stale);
    }
    // 1. A record, a disposition or the head moved.
    let head: Option<(String, i64)> = db
        .prepare_cached("SELECT receipt_id, disposition_version FROM report_heads WHERE run_id=?1")?
        .query_row([run], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let disposition = head.as_ref().map_or(0, |head| head.1);
    if sequence(db, run)? != guard.sequence
        || disposition != guard.disposition_revision.as_sqlite_integer()
        || head.as_ref().map(|head| head.0.as_str()) != guard.head.as_ref().map(ReceiptId::as_str)
    {
        return Ok(PublishOutcome::Stale);
    }
    let pass = passes::current(db, run)?;
    let pass_id = pass.as_ref().map(|row| row.id);
    // 2. A pass opened after an update's snapshot.
    if matches!(kind, CommitKind::Update(_)) && pass_id != guard.pass {
        return Ok(PublishOutcome::Stale);
    }
    // 3. The authority, or a closing call's pass or batch changed.
    let authority = for_project(db, &identity_for_review(db, guard.lead.review_id())?)?;
    if authority.as_ref() != Some(&guard.authority) {
        return Err(AppError::Conflict.into());
    }
    let closing = match kind {
        CommitKind::Update(version) => return update(tx, &guard, *version),
        CommitKind::Closing(closing) => closing,
    };
    if pass_id != guard.pass
        || pass.as_ref().and_then(|row| row.batch.as_ref()) != guard.batch.as_ref()
    {
        return Err(AppError::Conflict.into());
    }
    // 4. The assessments must cover the frozen batch.
    let manifest = guard
        .batch
        .as_deref()
        .map(|batch| completion::frozen(db, batch))
        .transpose()?;
    if let Some(manifest) = &manifest {
        let mut errors = completion::assessment_errors(manifest, &closing.assessments);
        if let Some(version) = &closing.version {
            errors.extend(completion::edit_errors(
                db,
                run,
                manifest,
                &version.changes,
                &closing.assessments,
            )?);
        }
        if !errors.is_empty() {
            return Ok(PublishOutcome::Invalid(errors));
        }
    }
    // 5. Store it.
    close(tx, &guard, *closing, manifest.as_ref())
}

fn update(
    tx: &mut WriteTx<'_>,
    guard: &SnapshotGuard,
    version: NewVersion,
) -> Result<PublishOutcome, StoreError> {
    let time = now_ms()?;
    let (receipt, _) = store_version(&tx.sql, guard, &version, time)?;
    finish(
        tx,
        &guard.lead,
        receipt.as_str(),
        time,
        version.facts.new_attention.then_some(Ready::Report),
    )?;
    Ok(PublishOutcome::Published(receipt))
}

fn close(
    tx: &mut WriteTx<'_>,
    guard: &SnapshotGuard,
    closing: ClosingCommit,
    manifest: Option<&FrozenManifest>,
) -> Result<PublishOutcome, StoreError> {
    let ClosingCommit {
        call,
        summary,
        assessments,
        version,
    } = closing;
    let time = now_ms()?;
    let run = guard.lead.run_id().as_str();
    let (receipt, sha256, ready, outcome) = match &version {
        Some(version) => {
            let (receipt, sha256) = store_version(&tx.sql, guard, version, time)?;
            let ready = version.facts.new_attention.then_some(Ready::Report);
            (
                receipt.clone(),
                sha256,
                ready,
                PublishOutcome::Published(receipt),
            )
        }
        None => {
            let receipt = guard.head.clone().ok_or(AppError::Conflict)?;
            let sha256: String = tx
                .sql
                .prepare_cached(
                    "SELECT a.sha256 FROM report_versions v JOIN artifacts a USING(artifact_id)
                     WHERE v.run_id=?1 AND v.receipt_id=?2",
                )?
                .query_row(params![run, receipt.as_str()], |row| row.get(0))?;
            let ready = manifest.map(|manifest| Ready::Followup(manifest.batch_id.clone()));
            (receipt, parse(&sha256)?, ready, PublishOutcome::Unchanged)
        }
    };
    if let Some(pass) = guard.pass {
        tx.sql
            .prepare_cached(
                "UPDATE review_passes SET state='closed', closed_by='publish', closed_receipt_id=?2,
                   closed_at_ms=?3 WHERE pass_id=?1 AND state<>'closed'",
            )?
            .execute(params![pass.get(), receipt.as_str(), time])?;
    }
    let ack = FinishReceipt {
        receipt: receipt.clone(),
        sha256,
        summary,
    };
    calls::store(
        &tx.sql,
        &call,
        Tool::FinishReview,
        RUN_TARGET,
        guard.sequence,
        &finish_json(&ack, &assessments),
    )?;
    if let Some(manifest) = manifest {
        let kind = if version.is_some() {
            ResultKind::Final
        } else {
            ResultKind::Unchanged
        };
        completion::complete(
            tx,
            &guard.lead,
            kind,
            &assessments,
            manifest,
            receipt.as_str(),
            time,
        )?;
    }
    if let Some(turn) = &call.turn {
        complete_turn(tx, &guard.lead, turn)?;
    }
    finish(tx, &guard.lead, receipt.as_str(), time, ready)?;
    Ok(outcome)
}

/// Which ready notification a commit raises.
enum Ready {
    Report,
    /// An unchanged batch result, by its batch.
    Followup(String),
}

/// Writes the text, appends the version with its rows, moves the head, binds the disposition
/// revisions and marks the guarded sequence published.
fn store_version(
    db: &Connection,
    guard: &SnapshotGuard,
    version: &NewVersion,
    time: i64,
) -> Result<(ReceiptId, ReportHash), StoreError> {
    let run = guard.lead.run_id().as_str();
    let receipt: ReceiptId = parse(&Uuid::new_v4().to_string())?;
    let artifact = Uuid::new_v4().to_string();
    let composed = &version.composed;
    artifact::write(
        db,
        &artifact,
        &composed.text,
        &composed.sha256,
        &composed.stats,
    )?;
    let facts = &version.facts;
    let parent_attention: Option<String> = match &guard.head {
        Some(head) => db
            .prepare_cached(
                "SELECT attention_receipt_id FROM report_versions WHERE run_id=?1 AND receipt_id=?2",
            )?
            .query_row(params![run, head.as_str()], |row| row.get(0))
            .optional()?,
        None => None,
    };
    let attention = match parent_attention {
        Some(parent) if !facts.new_attention => parent,
        _ => receipt.as_str().to_owned(),
    };
    let (reviewed_sha, revision_reason) = match &facts.reviewed {
        RevisionEvidence::Exact(sha) => (Some(sha.as_str()), None),
        RevisionEvidence::Unknown(reason) => (None, Some(reason.as_str())),
    };
    let disposition = guard.disposition_revision.as_sqlite_integer();
    db.prepare_cached(
        "INSERT INTO report_versions(run_id, receipt_id, artifact_id, kind, ledger_sequence,
           parent_receipt_id, attention_receipt_id, reviewed_sha, revision_reason,
           full_mr_reviewed_sha, coverage, coverage_source_sha, coverage_target_sha, verdict,
           finding_count, change_count, published_at_ms, checked_at_ms, starting_disposition_version)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
    )?
    .execute(params![
        run,
        receipt.as_str(),
        artifact,
        facts.kind.as_str(),
        guard.sequence.as_sqlite_integer(),
        guard.head.as_ref().map(ReceiptId::as_str),
        attention,
        reviewed_sha,
        revision_reason,
        facts.full_mr_reviewed_sha.as_ref().map(|sha| sha.as_str()),
        facts.coverage.to_string(),
        facts.coverage_source_sha.as_ref().map(|sha| sha.as_str()),
        facts.coverage_target_sha.as_ref().map(|sha| sha.as_str()),
        facts.verdict.to_string(),
        composed.cards.len() as i64,
        version.changes.findings.len() as i64,
        time,
        facts.checked_at_ms,
        disposition,
    ])?;
    rows(db, run, receipt.as_str(), version)?;
    if let Some(head) = &guard.head {
        db.prepare_cached(
            "UPDATE report_versions SET closing_disposition_version=?3 WHERE run_id=?1 AND receipt_id=?2",
        )?
        .execute(params![run, head.as_str(), disposition])?;
    }
    db.prepare_cached(
        "INSERT INTO report_heads(run_id, receipt_id, disposition_version) VALUES(?1, ?2, ?3)
         ON CONFLICT(run_id) DO UPDATE SET receipt_id=excluded.receipt_id,
           disposition_version=excluded.disposition_version",
    )?
    .execute(params![run, receipt.as_str(), disposition])?;
    db.prepare_cached(
        "INSERT INTO ledger_runs(run_id, sequence, published_sequence) VALUES(?1, ?2, ?2)
         ON CONFLICT(run_id) DO UPDATE SET published_sequence=excluded.published_sequence",
    )?
    .execute(params![run, guard.sequence.as_sqlite_integer()])?;
    Ok((receipt, composed.sha256.clone()))
}

/// The version's finding, section and outcome rows. Each card's columns come from the record
/// at the guarded sequence.
fn rows(db: &Connection, run: &str, receipt: &str, version: &NewVersion) -> Result<(), StoreError> {
    let text = &version.composed.text;
    let evidence = |range: &std::ops::Range<usize>| -> Result<String, StoreError> {
        let slice = text.get(range.clone()).ok_or(AppError::Internal)?;
        Ok(
            ReportHash::from_sha256(Sha256::digest(slice.as_bytes()).into())
                .as_str()
                .to_owned(),
        )
    };
    let mut record = db.prepare_cached(
        "SELECT c.revision, c.title, c.severity, c.blocking
         FROM ledger_findings f JOIN ledger_changes c ON c.run_id=f.run_id AND c.sequence=f.sequence
         WHERE f.run_id=?1 AND f.finding=?2 AND f.active=1",
    )?;
    let mut finding = db.prepare_cached(
        "INSERT INTO report_findings(run_id, receipt_id, finding_id, record_revision, title, severity,
           blocking, scalar_offset, scalar_length, evidence_sha256)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for card in &version.composed.cards {
        let (revision, title, severity, blocking): (i64, String, String, bool) = record
            .query_row(params![run, card.id.number()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .optional()?
            .ok_or(AppError::Internal)?;
        finding.execute(params![
            run,
            receipt,
            card.id.to_string(),
            revision,
            title,
            severity,
            blocking,
            card.span.scalar_offset as i64,
            card.span.scalar_length as i64,
            evidence(&card.span.bytes)?,
        ])?;
    }
    let mut section = db.prepare_cached(
        "INSERT INTO report_sections(run_id, receipt_id, scalar_offset, scalar_length, title, evidence_sha256)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for span in &version.composed.sections {
        section.execute(params![
            run,
            receipt,
            span.span.scalar_offset as i64,
            span.span.scalar_length as i64,
            span.title,
            evidence(&span.span.bytes)?,
        ])?;
    }
    let mut change = db.prepare_cached(
        "INSERT INTO report_changes(run_id, receipt_id, finding_id, conclusion, reason, checked_sha)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut discussion = db.prepare_cached(
        "INSERT INTO report_change_discussions(run_id, receipt_id, finding_id, discussion_id)
         VALUES(?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
    )?;
    for edit in &version.changes.findings {
        change.execute(params![
            run,
            receipt,
            edit.finding_id,
            edit.conclusion.to_string(),
            edit.reason,
            edit.checked_sha.as_ref().map(|sha| sha.as_str()),
        ])?;
        for id in &edit.discussion_ids {
            discussion.execute(params![run, receipt, edit.finding_id, id])?;
        }
    }
    Ok(())
}

/// Resolves the run's failures and withdraws their notifications and those of earlier runs'
/// failures, raises the ready notification and emits the change events.
fn finish(
    tx: &mut WriteTx<'_>,
    lead: &LeadTarget,
    receipt: &str,
    time: i64,
    ready: Option<Ready>,
) -> Result<(), StoreError> {
    let run = lead.run_id().as_str();
    let mut alerts = failures::resolve(&tx.sql, run, receipt, time)?;
    alerts.extend(failures::unresolved_alerts(
        &tx.sql,
        lead.review_id(),
        Some(run),
    )?);
    let mut changed = withdraw(&tx.sql, alerts)?;
    let notification = match ready {
        Some(ready) => {
            let (kind, label, occurrence) = match ready {
                Ready::Report => (Kind::ReportReady, "Report ready", receipt.to_owned()),
                Ready::Followup(batch) => (
                    Kind::FollowupReady,
                    "Review follow-up ready",
                    format!("followup:{batch}"),
                ),
            };
            insert_notification(
                &tx.sql,
                Observation {
                    source: Source::Report,
                    occurrence_id: occurrence.clone(),
                    kind,
                    review: lead.review_id().clone(),
                    run: Some(lead.run_id().clone()),
                    session: Some(lead.session_key().clone()),
                    event_ref: Some(occurrence),
                    label: label.to_owned(),
                    occurred_at_ms: time,
                    desktop_episode: None,
                },
            )?
        }
        None => None,
    };
    changed.extend(notification);
    publish(tx, &changed)?;
    advance_queue(&tx.sql)?;
    tx.emit(&[
        ResourceKey::Queue,
        ResourceKey::Review(Some(lead.review_id().clone())),
        ResourceKey::Run(Some(lead.run_id().clone())),
        ResourceKey::Report(Some(lead.run_id().clone())),
    ])?;
    Ok(())
}

/// Builds the display index of a version that just committed.
pub(super) async fn index(
    store: &SqliteStore,
    run: RunId,
    receipt: ReceiptId,
) -> Result<(), StoreError> {
    store.cache_report_display(run, receipt).await
}
