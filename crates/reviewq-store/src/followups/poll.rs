use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::followups::{FollowupObservationStore, MonitoredReview};
use reviewq_app::queue::MergeRequestState;
use reviewq_app::sources::WorkflowSnapshot;
use reviewq_domain::identity::ReviewId;
use rusqlite::{Connection, params};

use super::{observations, revision};
use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::queue::{identity_for_review, parse, save_observations, text};
use crate::workspace::{advance_queue, configured, settings_revision};
use crate::{SqliteStore, StoreError};

const ACTIVE: &str = "
   m.binding_verified=1 AND m.state='enabled' AND m.run_id IS NOT NULL
       AND coalesce(m.policy_override, (SELECT automatic FROM followup_preferences WHERE singleton=1), 0)=1
       AND EXISTS(SELECT 1 FROM configured_projects p WHERE p.origin=r.origin AND p.project_id=r.project_id)";

impl FollowupObservationStore for SqliteStore {
    fn monitored_reviews(
        &self,
        after: Option<ReviewId>,
        limit: u16,
    ) -> AppFuture<'_, Vec<MonitoredReview>> {
        Box::pin(async move {
            if !(1..=32).contains(&limit) {
                return Err(AppError::InvalidRequest);
            }
            self.read(move |db| list(db, after, limit))
                .await
                .map_err(StoreError::app)
        })
    }

    fn record_monitor_observation(
        &self,
        target: MonitoredReview,
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
) -> Result<Vec<MonitoredReview>, StoreError> {
    let settings = settings_revision(db)?;
    let sql = format!(
        "SELECT m.review_id,m.run_id,m.generation FROM followup_monitors m JOIN reviews r USING(review_id)
         WHERE m.review_id>?1 AND {ACTIVE} ORDER BY m.review_id LIMIT ?2");
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query(params![after.as_ref().map_or("", |id| id.as_str()), limit])?;
    let mut items = Vec::new();
    while let Some(row) = rows.next()? {
        let review = parse(text(row, 0)?)?;
        items.push(MonitoredReview {
            identity: identity_for_review(db, &review)?,
            review,
            run: parse(text(row, 1)?)?,
            generation: revision(row.get(2)?)?,
            settings,
        });
    }
    Ok(items)
}

fn record(
    tx: &mut WriteTx<'_>,
    target: MonitoredReview,
    observation: WorkflowSnapshot,
) -> Result<(), StoreError> {
    let review = &target.review;
    let db = &tx.sql;
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM followup_monitors m JOIN reviews r USING(review_id)
           WHERE m.review_id=?1 AND m.run_id=?2 AND m.generation=?3 AND {ACTIVE})"
    );
    let unchanged: bool = db.query_row(
        &sql,
        params![
            review.as_str(),
            target.run.as_str(),
            target.generation.as_sqlite_integer()
        ],
        |row| row.get(0),
    )?;
    if !unchanged
        || target.settings != settings_revision(db)?
        || !configured(db, &target.identity)?
        || observation.review.identity != target.identity
    {
        return Err(AppError::StaleRevision.into());
    }
    let local_updated: i64 = db.query_row(
        "SELECT updated_at_ms FROM reviews WHERE review_id=?1",
        [review.as_str()],
        |row| row.get(0),
    )?;
    if local_updated > observation.review.updated_at_ms {
        return Err(AppError::StaleRevision.into());
    }
    save_observations(tx, std::slice::from_ref(&observation.review))?;
    let db = &tx.sql;
    let now = now_ms()?;
    let mut keys = vec![
        ResourceKey::Followup(Some(review.clone())),
        ResourceKey::Review(Some(review.clone())),
        ResourceKey::Queue,
    ];
    let reviewer: Option<String> = db.query_row(
        "SELECT reviewer_id FROM followup_monitors WHERE review_id=?1",
        [review.as_str()],
        |row| row.get(0),
    )?;
    if reviewer.as_deref() != Some(observation.authenticated_actor.to_string().as_str()) {
        return hold(tx, review, now, "monitored_reviewer_changed");
    }
    let stopped = match (&observation.review.state, observation.personal_approval) {
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
    } else if observation.personal_approval.is_none() {
        return hold(tx, review, now, "reviewer_approval_unknown");
    } else {
        if let Err(error) =
            observations::require_known_threads(db, review, target.generation, &observation)
        {
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
        let evidence = observations::adopt_remote(db, &target, &observation, now)?;
        if evidence.is_empty() && changed == 0 {
            return Ok(());
        }
        if !evidence.is_empty() {
            if evidence.len() <= 29 {
                keys.extend(
                    evidence
                        .into_iter()
                        .map(|id| ResourceKey::Notification(Some(id))),
                );
            } else {
                keys.push(ResourceKey::Notification(None));
            }
        }
    }
    advance_queue(&tx.sql)?;
    tx.emit(&keys)?;
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
