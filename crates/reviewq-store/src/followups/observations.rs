use reviewq_app::access::AppError;
use reviewq_app::followups::Selection;
use reviewq_app::sources::{WorkflowDiscussion, WorkflowSnapshot};
use reviewq_domain::identity::{ReportHash, ReviewId, Revision, RunId};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::StoreError;
use crate::queue::lead_for_run;
use crate::workspace::hash_field;

pub(super) struct VerifiedThread<'a> {
    original: u64,
    discussion: &'a WorkflowDiscussion,
    finding_ids: &'a [String],
}

pub(super) fn verified_threads<'a>(
    snapshot: &'a WorkflowSnapshot,
    selections: &'a [Selection],
) -> Result<Vec<VerifiedThread<'a>>, StoreError> {
    let mut output = Vec::new();
    for selection in selections {
        let discussion = snapshot
            .discussions
            .iter()
            .find(|row| row.id == selection.discussion_id)
            .ok_or(AppError::Conflict)?;
        for original in &selection.original_note_ids {
            let owned = discussion
                .notes
                .iter()
                .any(|note| note.id == *original && note.author == snapshot.authenticated_actor);
            if !owned {
                return Err(AppError::Conflict.into());
            }
            output.push(VerifiedThread {
                original: *original,
                discussion,
                finding_ids: &selection.finding_ids,
            });
        }
    }
    Ok(output)
}

pub(super) fn save_selection(
    db: &Connection,
    review: &ReviewId,
    generation: Revision,
    reviewer: u64,
    threads: Vec<VerifiedThread<'_>>,
) -> Result<(), StoreError> {
    for thread in threads {
        let original = thread.original.to_string();
        let fingerprint = fingerprint(thread.discussion);
        db.prepare_cached(
            "INSERT INTO followup_threads(review_id, generation, original_note_id, discussion_id,
                reviewer_id, selected, discussion_version, baseline_sha256, resolved)
             VALUES(?1,?2,?3,?4,?5,1,'1',?6,?7)",
        )?
        .execute(params![
            review.as_str(),
            generation.as_sqlite_integer(),
            original,
            thread.discussion.id,
            reviewer.to_string(),
            fingerprint.as_str(),
            thread.discussion.resolved,
        ])?;
        for finding in thread.finding_ids {
            db.prepare_cached(
                "INSERT INTO followup_thread_findings(review_id,generation,original_note_id,finding_id)
                 VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",
            )?.execute(params![review.as_str(), generation.as_sqlite_integer(), original, finding])?;
        }
        save_notes(db, review, generation, &original, thread.discussion)?;
    }
    Ok(())
}

fn save_notes(
    db: &Connection,
    review: &ReviewId,
    generation: Revision,
    original: &str,
    discussion: &WorkflowDiscussion,
) -> Result<(), StoreError> {
    let mut insert = db.prepare_cached(
        "INSERT INTO followup_note_baselines(review_id,generation,original_note_id,note_id,reviewer_id,body_sha256)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(review_id,generation,original_note_id,note_id)
         DO UPDATE SET reviewer_id=excluded.reviewer_id, body_sha256=excluded.body_sha256",
    )?;
    for note in &discussion.notes {
        insert.execute(params![
            review.as_str(),
            generation.as_sqlite_integer(),
            original,
            note.id.to_string(),
            note.author.to_string(),
            note.body_hash.as_str(),
        ])?;
    }
    Ok(())
}

fn fingerprint(discussion: &WorkflowDiscussion) -> ReportHash {
    let mut notes: Vec<_> = discussion.notes.iter().collect();
    notes.sort_unstable_by_key(|note| note.id);
    let mut digest = Sha256::new();
    hash_field(&mut digest, discussion.id.as_bytes());
    digest.update([u8::from(discussion.resolved)]);
    for note in notes {
        digest.update(note.id.to_be_bytes());
        digest.update(note.author.to_be_bytes());
        hash_field(&mut digest, note.body_hash.as_str().as_bytes());
    }
    ReportHash::from_sha256(digest.finalize().into())
}

struct SelectionRow {
    original: String,
    version: String,
    reviewer: String,
}

fn selected(
    db: &Connection,
    review: &ReviewId,
    generation: Revision,
) -> Result<Vec<SelectionRow>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT original_note_id, discussion_version, reviewer_id FROM followup_threads
         WHERE review_id=?1 AND generation=?2 AND selected=1 ORDER BY original_note_id LIMIT 501",
    )?;
    let rows = statement.query_map(
        params![review.as_str(), generation.as_sqlite_integer()],
        |row| {
            Ok(SelectionRow {
                original: row.get(0)?,
                version: row.get(1)?,
                reviewer: row.get(2)?,
            })
        },
    )?;
    let output = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    if output.len() > 500 {
        return Err(AppError::PayloadTooLarge.into());
    }
    Ok(output)
}

fn exact_discussion<'a>(
    row: &SelectionRow,
    snapshot: &'a WorkflowSnapshot,
) -> Result<&'a WorkflowDiscussion, StoreError> {
    let id = row
        .original
        .parse::<u64>()
        .map_err(|_| AppError::Internal)?;
    let reviewer = row
        .reviewer
        .parse::<u64>()
        .map_err(|_| AppError::Internal)?;
    if reviewer != snapshot.authenticated_actor {
        return Err(AppError::Conflict.into());
    }
    snapshot
        .discussions
        .iter()
        .find(|discussion| {
            discussion
                .notes
                .iter()
                .any(|note| note.id == id && note.author == reviewer)
        })
        .ok_or_else(|| AppError::Conflict.into())
}

pub(super) fn require_known_threads(
    db: &Connection,
    review: &ReviewId,
    generation: Revision,
    snapshot: &WorkflowSnapshot,
) -> Result<(), StoreError> {
    for row in selected(db, review, generation)? {
        exact_discussion(&row, snapshot)?;
    }
    Ok(())
}

pub(super) fn review_now(
    db: &Connection,
    review: &ReviewId,
    run: &RunId,
    generation: Revision,
    submission: &str,
    snapshot: &WorkflowSnapshot,
    time: i64,
) -> Result<String, StoreError> {
    let selection = selected(db, review, generation)?;
    if selection.is_empty() {
        return Err(AppError::Conflict.into());
    }
    let batch = batch(db, review, run, generation, time, true)?;
    for row in selection {
        let discussion = exact_discussion(&row, snapshot)?;
        let observation = Uuid::new_v4().to_string();
        let fingerprint = fingerprint(discussion);
        db.prepare_cached(
            "INSERT INTO followup_observations(observation_id,review_id,generation,original_note_id,
                discussion_id,discussion_version,occurrence_key,kind,actor_id,observed_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,'requested',?8,?9)",
        )?.execute(params![
            observation, review.as_str(), generation.as_sqlite_integer(), row.original,
            discussion.id, format!("{}:{}", row.version, fingerprint), format!("request:{submission}"),
            snapshot.authenticated_actor.to_string(), snapshot.observed_at_ms,
        ])?;
        attach(db, review, &batch, &observation, None)?;
    }
    Ok(batch)
}

fn batch(
    db: &Connection,
    review: &ReviewId,
    run: &RunId,
    generation: Revision,
    time: i64,
    manual: bool,
) -> Result<String, StoreError> {
    let ready = if manual {
        time
    } else {
        time.saturating_add(60_000)
    };
    let existing = db
        .query_row(
            "SELECT batch_id FROM followup_batches
         WHERE review_id=?1 AND generation=?2 AND authority_verified=1 AND state='pending'",
            params![review.as_str(), generation.as_sqlite_integer()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        db.execute(
            "UPDATE followup_batches SET latest_observed_at_ms=max(latest_observed_at_ms,?2),
                due_at_ms=CASE WHEN manual_requested=1 THEN due_at_ms
                    ELSE min(?3,first_observed_at_ms+180000) END,
                manual_requested=max(manual_requested,?4)
             WHERE batch_id=?1",
            params![existing, time, ready, manual],
        )?;
        return Ok(existing);
    }
    let lead = lead_for_run(db, run)?.ok_or(AppError::NotFound)?;
    if lead.review_id() != review {
        return Err(AppError::NotFound.into());
    }
    let id = Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO followup_batches(batch_id,review_id,run_id,lead_session_key,generation,state,
              authority_verified,holds_slot,due_at_ms,first_observed_at_ms,latest_observed_at_ms,manual_requested)
         VALUES(?1,?2,?3,?4,?5,'pending',1,0,?6,?7,?7,?8)",
        params![id, review.as_str(), run.as_str(), lead.session_key().as_str(),
            generation.as_sqlite_integer(), ready, time, manual],
    )?;
    Ok(id)
}

fn attach(
    db: &Connection,
    review: &ReviewId,
    batch: &str,
    observation: &str,
    prior: Option<&str>,
) -> Result<(), StoreError> {
    db.prepare_cached(
        "INSERT INTO followup_members(review_id,batch_id,observation_id,reconciles_batch_id)
         VALUES(?1,?2,?3,?4) ON CONFLICT DO NOTHING",
    )?
    .execute(params![review.as_str(), batch, observation, prior])?;
    Ok(())
}

pub(super) fn carry_pending(
    db: &Connection,
    review: &ReviewId,
    from: Revision,
    to: Revision,
    run: &RunId,
    time: i64,
) -> Result<(), StoreError> {
    let old = db
        .query_row(
            "SELECT batch_id,manual_requested FROM followup_batches
         WHERE review_id=?1 AND generation=?2 AND authority_verified=1 AND state='pending'",
            params![review.as_str(), from.as_sqlite_integer()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()?;
    let Some((old, manual)) = old else {
        return Ok(());
    };
    let mut statement = db.prepare_cached(
        "SELECT n.original_note_id,n.discussion_id,o.discussion_version,o.occurrence_key,o.kind,
            o.actor_id,o.note_id,o.observed_at_ms
         FROM followup_members m JOIN followup_observations o USING(review_id,observation_id)
         JOIN followup_threads n ON n.review_id=o.review_id AND n.original_note_id=o.original_note_id
         WHERE m.review_id=?1 AND m.batch_id=?2 AND n.generation=?3 AND n.selected=1
           AND n.discussion_id=o.discussion_id ORDER BY o.observation_id",
    )?;
    let mut rows = statement.query(params![review.as_str(), old, to.as_sqlite_integer()])?;
    db.execute(
        "UPDATE followup_batches SET state='cancelled', hold_reason='monitor_reconfigured'
         WHERE batch_id=?1 AND state='pending'",
        [&old],
    )?;
    let mut next = None;
    while let Some(row) = rows.next()? {
        let id = Uuid::new_v4().to_string();
        db.prepare_cached(
            "INSERT INTO followup_observations(observation_id,review_id,generation,original_note_id,
                discussion_id,discussion_version,occurrence_key,kind,actor_id,note_id,observed_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        )?.execute(params![
            id, review.as_str(), to.as_sqlite_integer(),
            row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
            row.get::<_, String>(3)?, row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?, row.get::<_, Option<String>>(6)?, row.get::<_, i64>(7)?,
        ])?;
        let batch = match &next {
            Some(id) => id,
            None => next.insert(batch(db, review, run, to, time, manual)?),
        };
        attach(db, review, batch, &id, Some(&old))?;
    }
    Ok(())
}
