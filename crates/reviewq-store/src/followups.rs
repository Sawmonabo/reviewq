//! Scoped enrollment, exact retained batch outcomes and unread assessment pages.

mod commands;
mod observations;
mod poll;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::followups::{
    Assessment, AssessmentPage, AssessmentPosition, Batch, Enrollment, FollowupStore,
    MAX_ASSESSMENTS, MAX_RESULTS, MAX_SELECTED_NOTES, Monitor, Preferences, ResultPage,
    ResultPosition, ResultSummary, SelectedThread,
};
use reviewq_domain::identity::{ReviewId, Revision};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::events::snapshot;
use crate::queue::{identity_for_review, parse, text};
use crate::workspace::{actor_key, allow_review, scope};
use crate::{SqliteStore, StoreError};

impl FollowupStore for SqliteStore {
    fn read_followup_preferences(&self, actor: Actor) -> AppFuture<'_, Snapshot<Preferences>> {
        Box::pin(async move {
            self.read(move |db| {
                scope(db, &actor)?;
                snapshot(db, preferences(db)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_followups(
        &self,
        actor: Actor,
        review: ReviewId,
    ) -> AppFuture<'_, Snapshot<Enrollment>> {
        Box::pin(async move {
            self.read(move |db| {
                authorize(db, &actor, &review)?;
                let mut latest = results(db, &review, None, 1)?.items;
                let unviewed = db.query_row(
                    "SELECT count(*) FROM followup_results r
                     WHERE r.review_id=?1 AND NOT EXISTS (SELECT 1 FROM followup_result_reads v
                           WHERE v.viewer_key=?2 AND v.review_id=r.review_id AND v.batch_id=r.batch_id)",
                    params![review.as_str(), actor_key(&actor).as_slice()], |row| row.get::<_, i64>(0),
                )?;
                let value = Enrollment {
                    preferences: preferences(db)?,
                    monitor: monitor(db, &review)?,
                    batches: batches(db, &review)?,
                    latest: latest.pop(),
                    unviewed_batches: unsigned(unviewed)?,
                };
                snapshot(db, value)
            }).await.map_err(StoreError::app)
        })
    }

    fn read_followup_results(
        &self,
        actor: Actor,
        review: ReviewId,
        after: Option<ResultPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ResultPage>> {
        Box::pin(async move {
            if !(1..=MAX_RESULTS).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |db| {
                authorize(db, &actor, &review)?;
                snapshot(db, results(db, &review, after, limit)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn read_followup_assessments(
        &self,
        actor: Actor,
        review: ReviewId,
        through: Option<String>,
        after: Option<AssessmentPosition>,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<AssessmentPage>> {
        Box::pin(async move {
            if !(1..=MAX_ASSESSMENTS).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |db| {
                authorize(db, &actor, &review)?;
                snapshot(db, assessments(db, &actor, &review, through, after, limit)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn authorize(db: &Connection, actor: &Actor, review: &ReviewId) -> Result<(), StoreError> {
    let access = scope(db, actor)?;
    let identity = identity_for_review(db, review)?;
    allow_review(db, access, review, &identity)
}

fn preferences(db: &Connection) -> Result<Preferences, StoreError> {
    let (automatic, concurrency, value) = db
        .query_row(
            "SELECT automatic, active_limit, revision FROM followup_preferences WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .unwrap_or((false, 2, 0));
    Ok(Preferences {
        automatic,
        concurrency,
        revision: revision(value)?,
    })
}

fn monitor(db: &Connection, review: &ReviewId) -> Result<Option<Monitor>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT run_id, generation, state, policy_override, binding_verified, reason, updated_at_ms
         FROM followup_monitors WHERE review_id=?1",
    )?;
    let mut rows = statement.query([review.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let generation = revision(row.get(1)?)?;
    Ok(Some(Monitor {
        run: optional(row, 0)?,
        generation,
        state: parse(text(row, 2)?)?,
        policy_override: row.get(3)?,
        binding_verified: row.get(4)?,
        reason: row.get(5)?,
        updated_at_ms: row.get(6)?,
        selections: selections(db, review, generation)?,
    }))
}

fn selections(
    db: &Connection,
    review: &ReviewId,
    generation: Revision,
) -> Result<Vec<SelectedThread>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT t.original_note_id, t.discussion_id, t.reviewer_id, t.selected, t.discussion_version,
           t.resolved, (SELECT json_group_array(f.finding_id) FROM (
                SELECT finding_id FROM followup_thread_findings
                WHERE review_id=t.review_id AND generation=t.generation AND original_note_id=t.original_note_id
                ORDER BY finding_id LIMIT 50
           ) f)
         FROM followup_threads t WHERE t.review_id=?1 AND t.generation=?2
         ORDER BY t.original_note_id LIMIT ?3"
    )?;
    let mut rows = statement.query(params![
        review.as_str(),
        generation.as_sqlite_integer(),
        MAX_SELECTED_NOTES + 1
    ])?;
    let mut output = Vec::new();
    while let Some(row) = rows.next()? {
        if output.len() == usize::from(MAX_SELECTED_NOTES) {
            return Err(AppError::PayloadTooLarge.into());
        }
        output.push(SelectedThread {
            original_note_id: row.get(0)?,
            discussion_id: row.get(1)?,
            reviewer_id: row.get(2)?,
            selected: row.get(3)?,
            version: row.get(4)?,
            resolved: row.get(5)?,
            finding_ids: finding_ids(row, 6)?,
        });
    }
    Ok(output)
}

fn batches(db: &Connection, review: &ReviewId) -> Result<Vec<Batch>, StoreError> {
    let mut statement = db.prepare_cached(
        "SELECT b.batch_id, b.run_id, b.generation, b.state, b.authority_verified, b.source_sha,
            b.target_sha, b.submission_id, b.hold_reason, b.due_at_ms,
            b.first_observed_at_ms, b.latest_observed_at_ms, b.completed_at_ms,
            (SELECT count(*) FROM followup_members m WHERE m.review_id=b.review_id AND m.batch_id=b.batch_id)
         FROM followup_batches b WHERE b.review_id=?1
         ORDER BY b.latest_observed_at_ms DESC, b.batch_id DESC LIMIT 10"
    )?;
    let mut rows = statement.query([review.as_str()])?;
    let mut output = Vec::new();
    while let Some(row) = rows.next()? {
        output.push(Batch {
            id: row.get(0)?,
            run: optional(row, 1)?,
            generation: revision(row.get(2)?)?,
            state: parse(text(row, 3)?)?,
            verified: row.get(4)?,
            source_sha: optional(row, 5)?,
            target_sha: optional(row, 6)?,
            submission: optional(row, 7)?,
            hold_reason: row.get(8)?,
            due_at_ms: row.get(9)?,
            first_at_ms: row.get(10)?,
            latest_at_ms: row.get(11)?,
            completed_at_ms: row.get(12)?,
            member_count: unsigned(row.get(13)?)?,
        });
    }
    Ok(output)
}

fn results(
    db: &Connection,
    review: &ReviewId,
    after: Option<ResultPosition>,
    limit: u16,
) -> Result<ResultPage, StoreError> {
    let (time, id) = after.as_ref().map_or((None, ""), |p| {
        (Some(p.completed_at_ms), p.batch_id.as_str())
    });
    let mut statement = db.prepare_cached(
        "SELECT batch_id, run_id, receipt_id, kind, checked_sha, completed_at_ms
         FROM followup_results
         WHERE review_id=?1 AND (?2 IS NULL OR (completed_at_ms, batch_id)<(?2, ?3))
         ORDER BY completed_at_ms DESC, batch_id DESC LIMIT ?4",
    )?;
    let mut rows = statement.query(params![review.as_str(), time, id, limit + 1])?;
    let mut items = Vec::with_capacity(usize::from(limit));
    while let Some(row) = rows.next()? {
        if items.len() == usize::from(limit) {
            return Ok(ResultPage {
                items,
                has_more: true,
            });
        }
        items.push(ResultSummary {
            batch_id: row.get(0)?,
            run: parse(text(row, 1)?)?,
            report: optional(row, 2)?,
            kind: parse(text(row, 3)?)?,
            checked_sha: optional(row, 4)?,
            completed_at_ms: row.get(5)?,
        });
    }
    Ok(ResultPage {
        items,
        has_more: false,
    })
}

fn assessments(
    db: &Connection,
    actor: &Actor,
    review: &ReviewId,
    through: Option<String>,
    after: Option<AssessmentPosition>,
    limit: u16,
) -> Result<AssessmentPage, StoreError> {
    let pinned = match (through, &after) {
        (Some(through), Some(after)) if through != after.through_batch_id => {
            return Err(AppError::CursorExpired.into());
        }
        (Some(through), _) => Some(through),
        (None, Some(after)) => Some(after.through_batch_id.clone()),
        (None, None) => None,
    };
    let bound: Option<(i64, String)> = if let Some(requested) = pinned {
        Some(
            db.query_row(
                "SELECT ordinal, batch_id FROM followup_results WHERE review_id=?1 AND batch_id=?2",
                params![review.as_str(), requested],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(AppError::NotFound)?,
        )
    } else {
        db.query_row(
            "SELECT ordinal, batch_id FROM followup_results WHERE review_id=?1 ORDER BY ordinal DESC LIMIT 1",
            [review.as_str()], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?
    };
    let Some((through, through_id)) = bound else {
        return Ok(AssessmentPage {
            items: Vec::new(),
            through_batch_id: None,
            has_more: false,
        });
    };
    let mut statement = db.prepare_cached(
        "SELECT a.batch_id, r.receipt_id, a.discussion_id, a.conclusion, a.reason, a.next_action,
            r.ordinal, (SELECT json_group_array(f.finding_id) FROM (
                 SELECT finding_id FROM followup_assessment_findings
                 WHERE review_id=a.review_id AND batch_id=a.batch_id AND discussion_id=a.discussion_id
                 ORDER BY finding_id LIMIT 50
             ) f)
         FROM followup_results r JOIN followup_assessments a USING(review_id, batch_id)
         WHERE r.review_id=?1 AND r.ordinal<=?2 AND
            (r.ordinal>?3 OR (r.ordinal=?3 AND a.discussion_id>?4))
            AND NOT EXISTS (SELECT 1 FROM followup_result_reads v
                WHERE v.viewer_key=?5 AND v.review_id=r.review_id AND v.batch_id=r.batch_id)
         ORDER BY r.ordinal, a.discussion_id LIMIT ?6"
    )?;
    let (ordinal, id) = after.as_ref().map_or((0, ""), |position| {
        (position.ordinal, position.discussion_id.as_str())
    });
    let mut rows = statement.query(params![
        review.as_str(),
        through,
        ordinal,
        id,
        actor_key(actor).as_slice(),
        limit + 1,
    ])?;
    let mut items = Vec::with_capacity(usize::from(limit));
    let mut has_more = false;
    while let Some(row) = rows.next()? {
        if items.len() == usize::from(limit) {
            has_more = true;
            break;
        }
        items.push(Assessment {
            batch_id: row.get(0)?,
            receipt_id: optional(row, 1)?,
            discussion_id: row.get(2)?,
            conclusion: parse(text(row, 3)?)?,
            reason: row.get(4)?,
            next_action: parse(text(row, 5)?)?,
            ordinal: row.get(6)?,
            finding_ids: finding_ids(row, 7)?,
        });
    }
    Ok(AssessmentPage {
        items,
        through_batch_id: Some(through_id),
        has_more,
    })
}

fn finding_ids(row: &Row<'_>, column: usize) -> Result<Vec<String>, StoreError> {
    serde_json::from_str(text(row, column)?).map_err(|_| AppError::Internal.into())
}

fn optional<T: std::str::FromStr>(row: &Row<'_>, column: usize) -> Result<Option<T>, StoreError> {
    row.get::<_, Option<String>>(column)?
        .as_deref()
        .map(parse)
        .transpose()
}

fn revision(value: i64) -> Result<Revision, StoreError> {
    Revision::try_from(value).map_err(|_| AppError::Internal.into())
}

fn unsigned(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| AppError::Internal.into())
}
