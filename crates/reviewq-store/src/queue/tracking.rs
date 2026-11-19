use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::queue::{TicketAuthority, TrackReview, TrackedReviewReceipt, TrackingStore};
use reviewq_domain::identity::TicketKey;
use rusqlite::{Connection, params};

use super::{check_observation, save_observations, source_for_review};
use crate::events::snapshot;
use crate::workspace::{advance_queue, configured, operator, settings_revision};
use crate::{SqliteStore, StoreError};

impl TrackingStore for SqliteStore {
    fn track(
        &self,
        actor: Actor,
        input: TrackReview,
    ) -> AppFuture<'_, Snapshot<TrackedReviewReceipt>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.write(move |tx| {
                let db = &tx.sql;
                if settings_revision(db)? != input.settings {
                    return Err(AppError::StaleRevision.into());
                }
                if !configured(db, &input.review.identity)? {
                    return Err(AppError::NotFound.into());
                }
                check_observation(db, input.before, &input.review)?;
                let id = save_observations(tx, std::slice::from_ref(&input.review))?
                    .into_iter().next().ok_or(AppError::Internal)?;
                let review = source_for_review(&tx.sql, &id)?;
                if review.head_sha != input.review.head_sha {
                    return Err(AppError::Conflict.into());
                }
                let mut changed = tx.sql.execute(
                    "INSERT INTO review_tracking VALUES (?1) ON CONFLICT DO NOTHING",
                    [id.as_str()],
                )? != 0;
                for ticket in input.tickets {
                    changed |= tx.sql.prepare_cached(
                        "INSERT INTO retained_review_tickets VALUES (?1,?2,?3)
                         ON CONFLICT(review_id,ticket_key) DO UPDATE SET title=excluded.title WHERE title<>excluded.title",
                    )?.execute(params![id.as_str(),ticket.key.as_str(),ticket.title])? != 0;
                }
                if changed {
                    advance_queue(&tx.sql)?;
                    tx.emit(&[ResourceKey::Queue, ResourceKey::Review(Some(id.clone())), ResourceKey::Ticket(None)])?;
                }
                snapshot(&tx.sql, TrackedReviewReceipt {id, review})
            }).await.map_err(StoreError::app)
        })
    }

    fn ticket_authority(
        &self,
        actor: Actor,
        key: TicketKey,
    ) -> AppFuture<'_, Snapshot<TicketAuthority>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(move |db| {
                let present = db.prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM scoped_issues WHERE ticket_key=?1)
                     OR EXISTS(SELECT 1 FROM retained_review_tickets WHERE ticket_key=?1)",
                )?.query_row([key.as_str()], |row| row.get::<_, bool>(0))?;
                if !present {
                    return Err(AppError::NotFound.into());
                }
                let mut stmt = db.prepare_cached(
                    "SELECT DISTINCT b.estimate_field FROM scoped_issues i JOIN settings_boards b USING(board_id)
                     WHERE i.ticket_key=?1 AND b.estimate_field IS NOT NULL ORDER BY b.estimate_field",
                )?;
                let estimate_fields = stmt.query_map([key.as_str()], |row| row.get(0))?
                    .collect::<Result<Vec<String>,_>>()?;
                snapshot(db, TicketAuthority { settings:settings_revision(db)?, estimate_fields })
            }).await.map_err(StoreError::app)
        })
    }
}

pub(crate) fn retain_current_tickets(db: &Connection) -> Result<(), StoreError> {
    db.execute(
        "INSERT INTO retained_review_tickets(review_id,ticket_key,title)
         SELECT si.review_id,si.ticket_key,min(i.title) FROM scoped_review_issues si
         JOIN scoped_issues i USING(board_id,ticket_key)
         WHERE EXISTS (SELECT 1 FROM retained_reviews WHERE review_id=si.review_id)
         GROUP BY si.review_id,si.ticket_key
         ON CONFLICT(review_id,ticket_key) DO UPDATE SET title=excluded.title WHERE title<>excluded.title",
        [],
    )?;
    Ok(())
}
