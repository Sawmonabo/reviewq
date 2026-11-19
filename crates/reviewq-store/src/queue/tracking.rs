use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::queue::{TicketAuthority, TrackReview, TrackedReviewReceipt, TrackingStore};
use reviewq_domain::identity::{EventCursor, TicketKey};
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
                    let affected = tx.sql.prepare_cached(
                        "INSERT INTO retained_review_tickets VALUES (?1,?2,?3,
                           (SELECT event_head+1 FROM service_state WHERE singleton=1))
                         ON CONFLICT(review_id,ticket_key) DO UPDATE SET title=excluded.title,verified_cursor=excluded.verified_cursor
                         WHERE title<>excluded.title AND verified_cursor<=?4",
                    )?.execute(params![id.as_str(),ticket.key.as_str(),ticket.title,input.before.as_sqlite_integer()])? != 0;
                    if !affected {
                        let same=tx.sql.prepare_cached(
                            "SELECT title=?3 FROM retained_review_tickets WHERE review_id=?1 AND ticket_key=?2"
                        )?.query_row(params![id.as_str(),ticket.key.as_str(),ticket.title],|row|row.get::<_,bool>(0))?;
                        if !same {
                            return Err(AppError::StaleRevision.into());
                        }
                    }
                    changed |= affected;
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
                let project = key.as_str().rsplit_once('-').map_or("", |(project, _)| project);
                let mut stmt = db.prepare_cached(
                    "SELECT b.estimate_field FROM scoped_issues i JOIN settings_boards b USING(board_id)
                     WHERE i.ticket_key=?1 AND b.estimate_field IS NOT NULL
                     UNION
                     SELECT b.estimate_field FROM settings_boards b
                     WHERE b.enabled=1 AND b.estimate_field IS NOT NULL
                       AND NOT EXISTS (SELECT 1 FROM scoped_issues WHERE ticket_key=?1)
                       AND (json_array_length(b.jira_projects)=0 OR ?2 IN (SELECT value FROM json_each(b.jira_projects)))
                       AND EXISTS (
                           SELECT 1 FROM retained_review_tickets t JOIN reviews r USING(review_id)
                           JOIN configured_projects p USING(origin,project_id)
                           JOIN settings_repositories repo USING(project_id)
                           WHERE t.ticket_key=?1 AND (repo.board_selection='any'
                               OR r.project_id IN (SELECT value FROM json_each(b.projects)))
                       )
                     ORDER BY 1",
                )?;
                let estimate_fields = stmt.query_map(params![key.as_str(), project], |row| row.get(0))?
                    .collect::<Result<Vec<String>,_>>()?;
                snapshot(db, TicketAuthority { settings:settings_revision(db)?, estimate_fields })
            }).await.map_err(StoreError::app)
        })
    }
}

pub(crate) fn retain_current_tickets(
    db: &Connection,
    observed: Option<EventCursor>,
) -> Result<bool, StoreError> {
    let changed = db.execute(
        "INSERT INTO retained_review_tickets(review_id,ticket_key,title,verified_cursor)
         SELECT si.review_id,si.ticket_key,min(i.title),coalesce(?1,0) FROM scoped_review_issues si
         JOIN scoped_issues i USING(board_id,ticket_key)
         WHERE EXISTS (SELECT 1 FROM retained_reviews WHERE review_id=si.review_id)
         GROUP BY si.review_id,si.ticket_key
         ON CONFLICT(review_id,ticket_key) DO UPDATE SET title=excluded.title,verified_cursor=excluded.verified_cursor
         WHERE ?1 IS NOT NULL AND title<>excluded.title AND verified_cursor<excluded.verified_cursor",
        [observed.map(EventCursor::as_sqlite_integer)],
    )?;
    Ok(changed != 0)
}
