//! What publishing composes from, read in one read transaction: the closing call's snapshot,
//! with its replay and refusal, and an update's.

use std::collections::{HashMap, HashSet};

use reviewq_app::access::AppError;
use reviewq_app::ledger::{
    CallBinding, ClosingRead, ClosingSnapshot, CoverFacts, LastClosing, LedgerSnapshot, NoteRecord,
    ParentFinding, ParentVersion, RefusedClosing, SnapshotFinding, SnapshotGuard,
};
use reviewq_app::reports::FindingDisposition;
use reviewq_domain::identity::{MergeRequestIid, Revision, RunId};
use reviewq_domain::ledger::{CallerRole, FindingId, Tool};
use reviewq_domain::session::LeadTarget;
use rusqlite::{Connection, OptionalExtension, params};

use super::calls::{self, finish_from};
use super::passes::{self, RUN_TARGET};
use super::rows::{self, FINDING_COLUMNS, FINDING_WIDTH, NOTE_COLUMNS, section_order};
use super::{caller, sequence};
use crate::StoreError;
use crate::events::now_ms;
use crate::queue::{identity_for_review, lead_for_run, parse, text};
use crate::reports::authority::for_project;

/// `finish_review`'s read: a replay, the snapshot of a call from the run's lead, or a refusal
/// the caller records.
pub(super) fn closing(db: &Connection, binding: &CallBinding) -> Result<ClosingRead, StoreError> {
    let caller = caller::resolve(db, binding)?;
    if caller.role() != CallerRole::Lead {
        return Err(AppError::Forbidden.into());
    }
    if let Some((stored, _)) = calls::replay(db, &caller, Tool::FinishReview, RUN_TARGET)? {
        return Ok(ClosingRead::Replayed(finish_from(&stored)?.0));
    }
    let Some(snapshot) = read(db, caller.lead.clone())? else {
        return Ok(ClosingRead::Refused(refused(
            db,
            caller.lead.run_id().clone(),
            binding,
        )?));
    };
    if let Some(batch) = &snapshot.guard.batch {
        let bound: bool = db
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM followup_batches WHERE batch_id=?1 AND run_id=?2
                   AND lead_session_key=?3 AND state='executing' AND authority_verified=1)",
            )?
            .query_row(
                params![
                    batch,
                    caller.lead.run_id().as_str(),
                    caller.lead.session_key().as_str()
                ],
                |row| row.get(0),
            )?;
        if !bound {
            return Ok(ClosingRead::Refused(refused(
                db,
                caller.lead.run_id().clone(),
                binding,
            )?));
        }
    }
    let last_closing = db
        .prepare_cached(
            "SELECT result, sequence FROM ledger_calls WHERE rowid=(SELECT max(rowid)
               FROM ledger_calls WHERE run_id=?1 AND tool='finish_review')",
        )?
        .query_row([caller.lead.run_id().as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .optional()?
        .map(|(result, sequence)| {
            let (ack, assessments) = finish_from(&rows::from_json(&result)?)?;
            Ok::<_, StoreError>(LastClosing {
                ack,
                sequence: Revision::try_from(sequence).map_err(|_| AppError::Internal)?,
                assessments,
            })
        })
        .transpose()?;
    Ok(ClosingRead::Current(Box::new(ClosingSnapshot {
        caller,
        last_closing,
        snapshot,
    })))
}

fn refused(
    db: &Connection,
    run: RunId,
    binding: &CallBinding,
) -> Result<RefusedClosing, StoreError> {
    let pass = passes::current(db, run.as_str())?.map(|row| row.id);
    Ok(RefusedClosing {
        sequence: sequence(db, run.as_str())?,
        run,
        turn: binding.turn.clone(),
        pass,
    })
}

/// An update's snapshot. `NotFound` for an unknown run, `Conflict` without authority.
pub(super) fn update(db: &Connection, run: &RunId) -> Result<LedgerSnapshot, StoreError> {
    let lead = lead_for_run(db, run)?.ok_or(AppError::NotFound)?;
    read(db, lead)?.ok_or_else(|| AppError::Conflict.into())
}

/// The snapshot of `lead`'s run, or `None` when its project no longer has authority.
fn read(db: &Connection, lead: LeadTarget) -> Result<Option<LedgerSnapshot>, StoreError> {
    let Some(authority) = for_project(db, &identity_for_review(db, lead.review_id())?)? else {
        return Ok(None);
    };
    let run = lead.run_id().as_str();
    let sequence = sequence(db, run)?;
    let head: Option<(String, i64)> = db
        .prepare_cached("SELECT receipt_id, disposition_version FROM report_heads WHERE run_id=?1")?
        .query_row([run], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let disposition_revision = Revision::try_from(head.as_ref().map_or(0, |head| head.1))
        .map_err(|_| AppError::Internal)?;
    let head = head.map(|(receipt, _)| parse(&receipt)).transpose()?;
    let parent = head
        .as_ref()
        .map(|receipt| crate::reports::version(db, lead.run_id(), receipt))
        .transpose()?
        .map(|version| ParentVersion {
            summary: version.summary,
            kind: version.kind,
            sequence: version.ledger_sequence,
            checked_at_ms: version.checked_at_ms,
        });
    let row = passes::current(db, run)?;
    let pass = row
        .as_ref()
        .map(|row| passes::describe(db, row))
        .transpose()?;
    let cover = cover(
        db,
        &lead,
        pass.as_ref().and_then(|pass| pass.batch.as_ref()),
    )?;
    let since = parent
        .as_ref()
        .map_or(0, |parent| parent.sequence.as_sqlite_integer());
    let touched = match &row {
        Some(row) => Touched {
            after_sequence: row.opened_sequence,
            from_ms: row.opened_at_ms,
        },
        None => Touched {
            after_sequence: since,
            from_ms: head
                .as_ref()
                .map(|receipt| published_at(db, run, receipt.as_str()))
                .transpose()?
                .unwrap_or(0),
        },
    };
    let findings = findings(
        db,
        &lead,
        head.as_ref().map(|receipt| receipt.as_str()),
        since,
        &touched,
        disposition_revision,
    )?;
    let notes = notes(db, run)?;
    Ok(Some(LedgerSnapshot {
        guard: SnapshotGuard {
            lead,
            authority,
            sequence,
            disposition_revision,
            head,
            pass: row.as_ref().map(|row| row.id),
            batch: row.and_then(|row| row.batch),
        },
        cover,
        parent,
        pass,
        findings,
        notes,
    }))
}

fn cover(
    db: &Connection,
    lead: &LeadTarget,
    batch: Option<&reviewq_app::ledger::FrozenBatch>,
) -> Result<CoverFacts, StoreError> {
    let (origin, project_path, iid, mr_title): (String, String, i64, String) = db
        .prepare_cached("SELECT origin, project_path, iid, title FROM reviews WHERE review_id=?1")?
        .query_row([lead.review_id().as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
    Ok(CoverFacts {
        mr_url: format!("{origin}/{project_path}/-/merge_requests/{iid}"),
        project_path,
        iid: MergeRequestIid::try_from(iid).map_err(|_| AppError::Internal)?,
        mr_title,
        target_sha: batch.and_then(|batch| batch.target_sha.clone()),
        read_at_ms: now_ms()?,
    })
}

/// Where the work a full review is checked against begins: the open pass's opening, or the
/// parent version's publication when no pass is open.
struct Touched {
    /// A finding whose change sequence is above this was changed or assessed in it.
    after_sequence: i64,
    /// An accepted `record_finding` or `assess_finding` call from this time on also touched
    /// its finding, even when it equalled the stored record and so advanced nothing.
    from_ms: i64,
}

fn published_at(db: &Connection, run: &str, receipt: &str) -> Result<i64, StoreError> {
    Ok(db
        .prepare_cached(
            "SELECT published_at_ms FROM report_versions WHERE run_id=?1 AND receipt_id=?2",
        )?
        .query_row(params![run, receipt], |row| row.get(0))?)
}

/// Every active finding, every finding on the parent version and every finding changed
/// since it, in ID order.
fn findings(
    db: &Connection,
    lead: &LeadTarget,
    head: Option<&str>,
    since: i64,
    touched: &Touched,
    disposition: Revision,
) -> Result<Vec<SnapshotFinding>, StoreError> {
    let run = lead.run_id().as_str();
    let mut parent: HashMap<u32, ParentFinding> = HashMap::new();
    if let Some(head) = head {
        let mut statement = db.prepare_cached(
            "SELECT finding_id, record_revision, blocking FROM report_findings
             WHERE run_id=?1 AND receipt_id=?2",
        )?;
        let mut found = statement.query(params![run, head])?;
        while let Some(row) = found.next()? {
            let id: FindingId = parse(text(row, 0)?)?;
            parent.insert(
                id.number(),
                ParentFinding {
                    revision: rows::revision(row.get(1)?)?,
                    blocking: row.get(2)?,
                },
            );
        }
    }
    let mut dispositions = dispositions(db, run, disposition)?;
    let mut discussions = discussions(db, lead)?;
    let parent_ids = serde_json::to_string(&parent.keys().collect::<Vec<_>>())
        .map_err(|_| AppError::Internal)?;
    let by_subagents: HashSet<i64> = db
        .prepare_cached(
            "SELECT finding FROM ledger_changes WHERE run_id=?1 AND sequence>?2
               AND finding IS NOT NULL AND author IS NOT NULL",
        )?
        .query_map(params![run, since], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let called: HashSet<i64> = db
        .prepare_cached(
            "SELECT json_extract(result, '$.id') FROM ledger_calls
             WHERE run_id=?1 AND tool IN ('record_finding','assess_finding') AND accepted_at_ms>=?2",
        )?
        .query_map(params![run, touched.from_ms], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let sql = format!(
        "SELECT {FINDING_COLUMNS}, f.sequence, f.outcome_sequence
         FROM ledger_findings f JOIN ledger_changes c ON c.run_id=f.run_id AND c.sequence=f.sequence
         WHERE f.run_id=?1 AND (f.active=1 OR f.sequence>?2
           OR f.finding IN (SELECT value FROM json_each(?3)))
         ORDER BY f.finding"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut found = statement.query(params![run, since, parent_ids])?;
    let mut findings = Vec::new();
    while let Some(row) = found.next()? {
        let record = rows::finding(row, 0)?;
        let changed: i64 = row.get(FINDING_WIDTH)?;
        let outcome: Option<i64> = row.get(FINDING_WIDTH + 1)?;
        let key = record.id.number();
        findings.push(SnapshotFinding {
            parent: parent.get(&key).copied(),
            outcome_is_new: outcome.is_some_and(|outcome| outcome > since),
            touched: changed > touched.after_sequence || called.contains(&i64::from(key)),
            by_subagent: by_subagents.contains(&i64::from(key)),
            disposition: dispositions.remove(&key),
            discussion_ids: discussions.remove(&key).unwrap_or_default(),
            record,
        });
    }
    Ok(findings)
}

/// The user's disposition of each finding at revision `at`, by finding number.
fn dispositions(
    db: &Connection,
    run: &str,
    at: Revision,
) -> Result<HashMap<u32, FindingDisposition>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT d.finding_id, d.blocking, d.publication, d.reason FROM finding_dispositions d
         WHERE d.run_id=?1 AND d.removed=0 AND d.disposition_version=(
           SELECT max(h.disposition_version) FROM finding_dispositions h
           WHERE h.run_id=d.run_id AND h.finding_id=d.finding_id AND h.disposition_version<=?2)",
    )?;
    let mut found = statement.query(params![run, at.as_sqlite_integer()])?;
    let mut dispositions = HashMap::new();
    while let Some(row) = found.next()? {
        let id: FindingId = parse(text(row, 0)?)?;
        dispositions.insert(
            id.number(),
            FindingDisposition {
                blocking: row.get(1)?,
                publication: rows::optional(row, 2)?,
                reason: row.get(3)?,
            },
        );
    }
    Ok(dispositions)
}

/// The discussions the run's follow-up monitor links to each finding, by finding number.
fn discussions(
    db: &Connection,
    lead: &LeadTarget,
) -> Result<HashMap<u32, Vec<String>>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT DISTINCT tf.finding_id, t.discussion_id FROM followup_monitors m
         JOIN followup_thread_findings tf ON tf.review_id=m.review_id AND tf.generation=m.generation
         JOIN followup_threads t ON t.review_id=tf.review_id AND t.generation=tf.generation
           AND t.original_note_id=tf.original_note_id
         WHERE m.review_id=?1 AND m.run_id=?2 AND t.discussion_id IS NOT NULL
         ORDER BY tf.finding_id, t.discussion_id",
    )?;
    let mut found = statement.query(params![lead.review_id().as_str(), lead.run_id().as_str()])?;
    let mut discussions: HashMap<u32, Vec<String>> = HashMap::new();
    while let Some(row) = found.next()? {
        let Ok(id) = text(row, 0)?.parse::<FindingId>() else {
            continue;
        };
        discussions
            .entry(id.number())
            .or_default()
            .push(row.get(1)?);
    }
    Ok(discussions)
}

/// The run's notes: fixed sections in their order, then extra notes in the order first filed.
fn notes(db: &Connection, run: &str) -> Result<Vec<NoteRecord>, StoreError> {
    let sql = format!(
        "SELECT {NOTE_COLUMNS}, n.first_sequence FROM ledger_notes n
         JOIN ledger_changes c ON c.run_id=n.run_id AND c.sequence=n.sequence
         WHERE n.run_id=?1 AND n.present=1"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut found = statement.query([run])?;
    let mut notes = Vec::new();
    while let Some(row) = found.next()? {
        let first: i64 = row.get(4)?;
        notes.push((first, rows::note(row, 0)?));
    }
    notes.sort_by_key(|(first, note)| (section_order(note.key.section()), *first));
    Ok(notes.into_iter().map(|(_, note)| note).collect())
}
