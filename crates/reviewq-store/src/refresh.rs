use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::sources::{
    MAX_PUBLICATION_REVIEWS, RefreshAttempt, SourceFailure, SourceObservation, SourceRefreshStore,
    SourceSync,
};
use reviewq_domain::identity::{MrIdentity, Revision};
use rusqlite::{Connection, OptionalExtension, params};

use crate::connection::WriteTx;
use crate::events::now_ms;
use crate::queue::{identity, mark_discovered, save_observations};
use crate::workspace::{advance_queue, configured, operator, settings_revision};
use crate::{SqliteStore, StoreError};

mod scope;

impl SourceRefreshStore for SqliteStore {
    fn begin_refresh(&self, actor: Actor, revision: Revision) -> AppFuture<'_, RefreshAttempt> {
        Box::pin(async move {
            self.write(move |tx| {
                operator(&actor)?;
                if settings_revision(&tx.sql)? != revision {
                    return Err(AppError::StaleRevision.into());
                }
                if read_sync(&tx.sql, revision)?.active {
                    return Err(AppError::Busy.into());
                }
                let id = tx.emit(&[ResourceKey::Queue])?;
                tx.sql.execute(
                    "INSERT INTO source_refresh(singleton,attempt,settings_revision,started_at_ms,active)
                     VALUES(1,?1,?2,?3,1)
                     ON CONFLICT(singleton) DO UPDATE SET
                         last_success_at_ms=CASE WHEN settings_revision=excluded.settings_revision
                             THEN last_success_at_ms ELSE NULL END,
                         error=CASE WHEN settings_revision=excluded.settings_revision
                             THEN error ELSE NULL END,
                         attempt=excluded.attempt,settings_revision=excluded.settings_revision,
                         started_at_ms=excluded.started_at_ms,active=1",
                    params![id.as_sqlite_integer(), revision.as_sqlite_integer(), now_ms()?],
                )?;
                Ok(RefreshAttempt { id, settings_revision: revision })
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn finish_refresh(
        &self,
        attempt: RefreshAttempt,
        result: Result<SourceObservation, SourceFailure>,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| finish(tx, attempt, result))
                .await
                .map_err(StoreError::app)
        })
    }

    fn retained_reviews(&self, attempt: RefreshAttempt) -> AppFuture<'_, Vec<MrIdentity>> {
        Box::pin(async move {
            self.read(move |db| {
                if settings_revision(db)? != attempt.settings_revision {
                    return Err(AppError::StaleRevision.into());
                }
                let mut query = db.prepare_cached(
                    "SELECT origin,project_id,iid FROM reviews AS r
                     WHERE r.origin=(SELECT origin FROM settings WHERE provider='gitlab')
                     AND r.state<>'merged' AND (
                         EXISTS (SELECT 1 FROM runs WHERE review_id=r.review_id)
                         OR EXISTS (SELECT 1 FROM review_decisions WHERE review_id=r.review_id)
                         OR EXISTS (SELECT 1 FROM notifications WHERE review_id=r.review_id))
                     ORDER BY r.review_id LIMIT ?1",
                )?;
                let mut rows = query.query([MAX_PUBLICATION_REVIEWS as i64 + 1])?;
                let mut retained = Vec::new();
                while let Some(row) = rows.next()? {
                    if retained.len() >= MAX_PUBLICATION_REVIEWS {
                        return Err(AppError::PayloadTooLarge.into());
                    }
                    retained.push(identity(row, 0)?);
                }
                Ok(retained)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn recover_refresh(&self, actor: Actor) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                operator(&actor)?;
                let updated = tx.sql.execute(
                    "UPDATE source_refresh SET active=0,error='interrupted' WHERE active=1",
                    [],
                )?;
                if updated != 0 {
                    tx.emit(&[ResourceKey::Queue])?;
                }
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn finish(
    tx: &mut WriteTx<'_>,
    attempt: RefreshAttempt,
    result: Result<SourceObservation, SourceFailure>,
) -> Result<(), StoreError> {
    if settings_revision(&tx.sql)? != attempt.settings_revision {
        return Err(AppError::StaleRevision.into());
    }
    let error = result.as_ref().err().map(|error| failure_label(*error));
    let updated = tx.sql.execute(
        "UPDATE source_refresh SET active=0,error=?3,
         last_success_at_ms=CASE WHEN ?3 IS NULL THEN ?4 ELSE last_success_at_ms END
         WHERE settings_revision=?1 AND attempt=?2 AND active=1",
        params![
            attempt.settings_revision.as_sqlite_integer(),
            attempt.id.as_sqlite_integer(),
            error,
            now_ms()?
        ],
    )?;
    if updated != 1 {
        return Err(AppError::StaleRevision.into());
    }
    if let Ok(source) = result {
        let SourceObservation {
            reviews,
            owned,
            issues,
            sprints,
        } = source;
        if reviews.len() > MAX_PUBLICATION_REVIEWS || owned.len() > MAX_PUBLICATION_REVIEWS {
            return Err(AppError::PayloadTooLarge.into());
        }
        let (rows, matches): (Vec<_>, Vec<_>) = reviews.into_values().unzip();
        for item in &rows {
            if !configured(&tx.sql, &item.identity)? {
                return Err(AppError::NotFound.into());
            }
        }
        let discovered = save_observations(tx, &rows)?;
        save_observations(tx, &owned)?;
        tx.sql.execute("DELETE FROM review_discoveries", [])?;
        mark_discovered(&tx.sql, &discovered)?;
        scope::replace(
            &tx.sql,
            issues,
            sprints,
            discovered.into_iter().zip(matches),
        )?;
        advance_queue(&tx.sql)?;
        tx.emit(&[ResourceKey::Ticket(None)])?;
    }
    tx.emit(&[ResourceKey::Queue])?;
    Ok(())
}

pub(crate) fn read_sync(
    connection: &Connection,
    revision: Revision,
) -> Result<SourceSync, StoreError> {
    let state = connection
        .query_row(
            "SELECT started_at_ms,last_success_at_ms,active,error FROM source_refresh
             WHERE settings_revision=?1",
            [revision.as_sqlite_integer()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((attempt, last_success_at_ms, active, error)) = state else {
        return Ok(SourceSync::default());
    };
    let error = match error.as_deref() {
        None => None,
        Some("unavailable") => Some(SourceFailure::Unavailable),
        Some("too_large") => Some(SourceFailure::TooLarge),
        Some("interrupted") => Some(SourceFailure::Interrupted),
        Some(_) => return Err(AppError::Internal.into()),
    };
    Ok(SourceSync {
        active,
        last_attempt_at_ms: Some(attempt),
        last_success_at_ms,
        error,
    })
}

fn failure_label(error: SourceFailure) -> &'static str {
    match error {
        SourceFailure::Unavailable => "unavailable",
        SourceFailure::TooLarge => "too_large",
        SourceFailure::Interrupted => "interrupted",
    }
}
