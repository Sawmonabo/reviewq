use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::followups::{FollowupObservationStore, MonitoredReview, TrackedReview};
use reviewq_app::queue::MergeRequestState;
use reviewq_app::sources::WorkflowSnapshot;
use reviewq_domain::identity::ReviewId;
use rusqlite::{Connection, OptionalExtension, params};

use super::{observations, revision};
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::notifications::{gitlab, publish};
use crate::queue::{identity_for_review, parse, save_observations, text};
use crate::workspace::{advance_queue, configured, settings_revision};
use crate::{SqliteStore, StoreError};

const ACTIVE: &str = "
   m.binding_verified=1 AND m.state='enabled' AND m.run_id IS NOT NULL
       AND coalesce(m.policy_override, (SELECT automatic FROM followup_preferences WHERE singleton=1), 0)=1
       AND EXISTS(SELECT 1 FROM configured_projects p WHERE p.origin=r.origin AND p.project_id=r.project_id)";

const ASSIGNED_RUN: &str = "coalesce(
    (SELECT x.run_id FROM runs x JOIN report_heads h USING(run_id)
      JOIN report_versions v USING(run_id,receipt_id) JOIN artifacts a USING(artifact_id)
      WHERE x.review_id=r.review_id AND a.sealed=1 AND a.complete=1
      ORDER BY v.captured_at_ms DESC,v.receipt_id DESC LIMIT 1),
    (SELECT min(x.run_id) FROM runs x WHERE x.review_id=r.review_id HAVING count(*)=1))";

impl FollowupObservationStore for SqliteStore {
    fn tracked_reviews(
        &self,
        after: Option<ReviewId>,
        limit: u16,
    ) -> AppFuture<'_, Vec<TrackedReview>> {
        Box::pin(async move {
            if !(1..=32).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |db| list(db, after, limit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn record_monitor_unavailable(&self, target: MonitoredReview) -> AppFuture<'_, ()> {
        Box::pin(async move {
            if target.generation.as_sqlite_integer() == 0 {
                return Ok(());
            }
            self.write(move |tx| {
                active(&tx.sql, &target)?;
                hold(tx, &target.review, now_ms()?, "followup_source_unavailable")
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn record_workflow_observation(
        &self,
        target: TrackedReview,
        observation: WorkflowSnapshot,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| record(tx, target, observation))
                .await
                .map_err(StoreError::app)
        })
    }
}

fn list(
    db: &Connection,
    after: Option<ReviewId>,
    limit: u16,
) -> Result<Vec<TrackedReview>, StoreError> {
    let settings = settings_revision(db)?;
    let sql = format!(
        "SELECT r.review_id,
           CASE WHEN {ACTIVE} THEN m.run_id
                WHEN m.review_id IS NULL AND (SELECT automatic FROM followup_preferences WHERE singleton=1)=1
                    THEN {ASSIGNED_RUN} END,
           CASE WHEN {ACTIVE} THEN m.generation ELSE 0 END
         FROM reviews r LEFT JOIN followup_monitors m USING(review_id)
         WHERE r.review_id>?1 AND (r.state='open' OR m.state='enabled')
           AND EXISTS(SELECT 1 FROM configured_projects p WHERE p.origin=r.origin AND p.project_id=r.project_id)
           AND (m.review_id IS NOT NULL OR EXISTS(SELECT 1 FROM runs x WHERE x.review_id=r.review_id)
              OR EXISTS(SELECT 1 FROM review_decisions x WHERE x.review_id=r.review_id)
              OR EXISTS(SELECT 1 FROM notifications x WHERE x.review_id=r.review_id))
         ORDER BY r.review_id LIMIT ?2"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query(params![after.as_ref().map_or("", |id| id.as_str()), limit])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let review = parse(text(row, 0)?)?;
        let identity = identity_for_review(db, &review)?;
        let run = row.get::<_, Option<String>>(1)?;
        let monitor = match run {
            Some(run) => Some(MonitoredReview {
                identity: identity.clone(),
                review: review.clone(),
                run: parse(&run)?,
                generation: revision(row.get(2)?)?,
                settings,
            }),
            None => None,
        };
        items.push(TrackedReview {
            review,
            identity,
            settings,
            monitor,
        });
    }
    Ok(items)
}

fn record(
    tx: &mut WriteTx<'_>,
    tracked: TrackedReview,
    observation: WorkflowSnapshot,
) -> Result<(), StoreError> {
    let db = &tx.sql;
    if tracked.settings != settings_revision(db)?
        || !configured(db, &tracked.identity)?
        || observation.review.identity != tracked.identity
        || identity_for_review(db, &tracked.review)? != tracked.identity
    {
        return Err(AppError::StaleRevision.into());
    }
    let local_updated: i64 = db.query_row(
        "SELECT updated_at_ms FROM reviews WHERE review_id=?1",
        [tracked.review.as_str()],
        |row| row.get(0),
    )?;
    if local_updated > observation.review.updated_at_ms {
        return Err(AppError::StaleRevision.into());
    }
    let reviewers = observations::monitored_reviewers(db, observation.authenticated_actor)?;
    let events = gitlab::collect(db, &tracked.review, &observation, &reviewers)?;
    save_observations(tx, std::slice::from_ref(&observation.review))?;
    publish(tx, &events)?;
    let Some(mut target) = tracked.monitor else {
        return Ok(());
    };
    let review = &target.review;
    let initial = target.generation.as_sqlite_integer() == 0;
    if !initial {
        match active(&tx.sql, &target).map_err(StoreError::app) {
            Err(AppError::StaleRevision) => return Ok(()),
            result => result?,
        }
    }
    let db = &tx.sql;
    let now = now_ms()?;
    let keys = [
        ResourceKey::Followup(Some(review.clone())),
        ResourceKey::Review(Some(review.clone())),
        ResourceKey::Queue,
    ];
    let approval = observation.reviewer_approval(&reviewers);
    if initial {
        if observation.review.state != MergeRequestState::Open || approval != Some(false) {
            return Ok(());
        }
        let sql = format!(
            "SELECT {ASSIGNED_RUN} FROM reviews r WHERE r.review_id=?1
               AND (SELECT automatic FROM followup_preferences WHERE singleton=1)=1
               AND NOT EXISTS(SELECT 1 FROM followup_monitors m WHERE m.review_id=r.review_id)"
        );
        let assigned: Option<String> = db
            .query_row(&sql, [review.as_str()], |row| row.get(0))
            .optional()?
            .flatten();
        if assigned.as_deref() != Some(target.run.as_str()) {
            return Ok(());
        }
        target.generation = revision(1)?;
        db.execute(
            "INSERT INTO followup_monitors(review_id,run_id,generation,state,reviewer_id,binding_verified,updated_at_ms,last_complete_observation_ms)
             VALUES(?1,?2,1,'enabled',?3,1,?4,?5)",
            params![review.as_str(),target.run.as_str(),observation.authenticated_actor.to_string(),now,observation.observed_at_ms],
        )?;
        observations::add_reviewer_threads(
            db,
            review,
            target.generation,
            &observation,
            &reviewers,
            None,
            true,
        )?;
        advance_queue(&tx.sql)?;
        tx.emit(&keys)?;
        return Ok(());
    }
    let (reviewer, previous_complete_at): (Option<String>, Option<i64>) = db.query_row(
        "SELECT reviewer_id,last_complete_observation_ms FROM followup_monitors WHERE review_id=?1",
        [review.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if previous_complete_at.is_some_and(|previous| previous > observation.observed_at_ms) {
        return Err(AppError::StaleRevision.into());
    }
    if reviewer.as_deref() != Some(observation.authenticated_actor.to_string().as_str()) {
        return hold(tx, review, now, "monitored_reviewer_changed");
    }
    let stopped = match (&observation.review.state, approval) {
        (MergeRequestState::Closed | MergeRequestState::Merged, _) => {
            Some(("archived", "merge_request_closed"))
        }
        (_, Some(true)) => Some(("paused", "reviewer_approval_confirmed")),
        _ => None,
    };
    if let Some((state, reason)) = stopped {
        db.execute(
            "UPDATE followup_monitors SET state=?2,reason=?3,updated_at_ms=max(updated_at_ms,?4)
             WHERE review_id=?1",
            params![review.as_str(), state, reason, now],
        )?;
        if state == "archived" {
            db.execute(
                "UPDATE followup_batches SET state='cancelled',hold_reason=?2
                 WHERE review_id=?1 AND authority_verified=1 AND state='pending'",
                params![review.as_str(), reason],
            )?;
        }
    } else if approval.is_none() {
        return hold(tx, review, now, "reviewer_approval_unknown");
    } else {
        if let Err(error) = observations::require_known_threads(
            db,
            review,
            target.generation,
            &observation,
            &reviewers,
        ) {
            if error.app() == AppError::Conflict {
                return hold(tx, review, now, "monitored_discussion_unavailable");
            }
            return Err(AppError::Internal.into());
        }
        let changed = db.execute(
            "UPDATE followup_monitors SET reason=NULL,updated_at_ms=max(updated_at_ms,?2)
             WHERE review_id=?1 AND reason IS NOT NULL",
            params![review.as_str(), now],
        )?;
        let selected = observations::add_reviewer_threads(
            db,
            review,
            target.generation,
            &observation,
            &reviewers,
            previous_complete_at,
            true,
        )?;
        let evidence = observations::adopt_remote(db, &target, &observation, &reviewers, now)?;
        db.execute(
            "UPDATE followup_monitors SET last_complete_observation_ms=?2 WHERE review_id=?1",
            params![review.as_str(), observation.observed_at_ms],
        )?;
        if !evidence && changed == 0 && !selected {
            return Ok(());
        }
    }
    advance_queue(&tx.sql)?;
    tx.emit(&keys)?;
    Ok(())
}

fn active(db: &Connection, target: &MonitoredReview) -> Result<(), StoreError> {
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM followup_monitors m JOIN reviews r USING(review_id)
           WHERE m.review_id=?1 AND m.run_id=?2 AND m.generation=?3 AND {ACTIVE})"
    );
    let unchanged: bool = db.query_row(
        &sql,
        params![
            target.review.as_str(),
            target.run.as_str(),
            target.generation.as_sqlite_integer()
        ],
        |row| row.get(0),
    )?;
    if !unchanged || target.settings != settings_revision(db)? {
        return Err(AppError::StaleRevision.into());
    }
    Ok(())
}

fn hold(tx: &mut WriteTx<'_>, review: &ReviewId, now: i64, reason: &str) -> Result<(), StoreError> {
    let changed = tx.sql.execute(
        "UPDATE followup_monitors SET reason=?2,updated_at_ms=max(updated_at_ms,?3)
         WHERE review_id=?1 AND reason IS NOT ?2",
        params![review.as_str(), reason, now],
    )?;
    if changed != 0 {
        advance_queue(&tx.sql)?;
        tx.emit(&[
            ResourceKey::Followup(Some(review.clone())),
            ResourceKey::Review(Some(review.clone())),
            ResourceKey::Queue,
        ])?;
    }
    Ok(())
}
