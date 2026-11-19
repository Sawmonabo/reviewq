use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::sources::{
    MAX_PUBLICATION_REVIEWS, RefreshAdmission, RefreshAttempt, RefreshReceipt, RefreshState,
    SourceFailure, SourceObservation, SourceRefreshStore, SourceSync,
};
use reviewq_domain::identity::{EventCursor, MrIdentity, Revision};
use rusqlite::{Connection, OptionalExtension, params};

use crate::connection::WriteTx;
use crate::events::{now_ms, snapshot};
use crate::queue::identity;
use crate::workspace::{operator, settings_revision};
use crate::{SqliteStore, StoreError};

mod scope;

impl SourceRefreshStore for SqliteStore {
    fn refresh_sync(
        &self,
        actor: Actor,
        settings: Revision,
    ) -> AppFuture<'_, Snapshot<SourceSync>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(move |db| {
                if settings_revision(db)? != settings {
                    return Err(AppError::StaleRevision.into());
                }
                snapshot(db, read_sync(db, settings)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn begin_refresh(&self, actor: Actor, revision: Revision) -> AppFuture<'_, RefreshAdmission> {
        Box::pin(async move {
            self.write(move |tx| {
                operator(&actor)?;
                if settings_revision(&tx.sql)? != revision {
                    return Err(AppError::StaleRevision.into());
                }
                let current = tx.sql.query_row(
                    "SELECT attempt FROM source_refresh WHERE settings_revision=?1 AND active=1",
                    [revision.as_sqlite_integer()],|row| row.get::<_,i64>(0),
                ).optional()?;
                if let Some(id) = current {
                    return Ok(RefreshAdmission { receipt: receipt(&tx.sql,cursor(id)?)?, started:false });
                }
                let now = now_ms()?;
                tx.sql.execute(
                    "UPDATE source_refresh_receipts SET state='superseded',finished_at_ms=?1
                     WHERE state='running'", [now],
                )?;
                let id = tx.emit(&[ResourceKey::QueueSync])?;
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
                    params![id.as_sqlite_integer(), revision.as_sqlite_integer(), now],
                )?;
                tx.sql.execute(
                    "INSERT INTO source_refresh_receipts(id,settings_revision,state) VALUES (?1,?2,'running')",
                    params![id.as_sqlite_integer(),revision.as_sqlite_integer()],
                )?;
                tx.sql.execute(
                    "DELETE FROM source_refresh_receipts WHERE id < coalesce(
                        (SELECT id FROM source_refresh_receipts ORDER BY id DESC LIMIT 1 OFFSET 511),0)",
                    [],
                )?;
                Ok(RefreshAdmission { receipt:receipt(&tx.sql,id)?, started:true })
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn refresh_receipt(
        &self,
        actor: Actor,
        id: EventCursor,
    ) -> AppFuture<'_, Snapshot<RefreshReceipt>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(move |db| receipt(db, id))
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
            self.write_prepared(
                move |db| {
                    scope::prepare(db, result.as_ref().ok())?;
                    Ok(result)
                },
                move |tx, result| {
                    finish(tx, attempt, result)?;
                    scope::clear(&tx.sql)
                },
            )
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
                     AND r.state<>'merged'
                     AND EXISTS (SELECT 1 FROM retained_reviews WHERE review_id=r.review_id)
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
                tx.sql.execute(
                    "UPDATE source_refresh_receipts SET state='failed',error='interrupted',finished_at_ms=?1
                     WHERE state='running'",[now_ms()?],
                )?;
                if updated != 0 {
                    tx.emit(&[ResourceKey::QueueSync])?;
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
    let now = now_ms()?;
    if settings_revision(&tx.sql)? != attempt.settings_revision {
        return supersede(tx, attempt, now);
    }
    let active = tx.sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM source_refresh WHERE attempt=?1 AND settings_revision=?2 AND active=1)",
        params![attempt.id.as_sqlite_integer(),attempt.settings_revision.as_sqlite_integer()],
        |row| row.get::<_,bool>(0),
    )?;
    if !active {
        return Ok(());
    }
    let (total, error) = match result {
        Ok(source) => match scope::publish(tx, attempt, source)? {
            Some(rows) => (Some(rows), None),
            None => return supersede(tx, attempt, now),
        },
        Err(error) => (None, Some(failure_label(error))),
    };
    tx.sql.execute(
        "UPDATE source_refresh SET active=0,error=?2,
         last_success_at_ms=CASE WHEN ?2 IS NULL THEN ?3 ELSE last_success_at_ms END
         WHERE attempt=?1",
        params![attempt.id.as_sqlite_integer(), error, now],
    )?;
    let terminal = if error.is_some() {
        "failed"
    } else {
        "succeeded"
    };
    let written = tx.sql.execute(
        "UPDATE source_refresh_receipts SET state=?2,finished_at_ms=?3,total_rows=?4,error=?5
         WHERE id=?1 AND settings_revision=?6 AND state='running'",
        params![
            attempt.id.as_sqlite_integer(),
            terminal,
            now,
            total,
            error,
            attempt.settings_revision.as_sqlite_integer()
        ],
    )?;
    if written != 1 {
        return Err(AppError::Internal.into());
    }
    tx.emit(&[ResourceKey::QueueSync])?;
    Ok(())
}

fn supersede(tx: &mut WriteTx<'_>, attempt: RefreshAttempt, now: i64) -> Result<(), StoreError> {
    let changed = tx.sql.execute(
        "UPDATE source_refresh_receipts SET state='superseded',finished_at_ms=?2 WHERE id=?1 AND state='running'",
        params![attempt.id.as_sqlite_integer(),now],
    )?;
    if changed != 0 {
        tx.sql.execute(
            "UPDATE source_refresh SET active=0 WHERE attempt=?1",
            [attempt.id.as_sqlite_integer()],
        )?;
        tx.emit(&[ResourceKey::QueueSync])?;
    }
    Ok(())
}

fn receipt(db: &Connection, id: EventCursor) -> Result<Snapshot<RefreshReceipt>, StoreError> {
    let value = db.query_row(
        "SELECT settings_revision,state,finished_at_ms,total_rows,error FROM source_refresh_receipts WHERE id=?1",
        [id.as_sqlite_integer()],|row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,
            row.get::<_,Option<i64>>(2)?,row.get::<_,Option<i64>>(3)?,row.get::<_,Option<String>>(4)?)),
    ).optional()?.ok_or(AppError::NotFound)?;
    let state = match value.1.as_str() {
        "running" => RefreshState::Running,
        "succeeded" => RefreshState::Succeeded,
        "failed" => RefreshState::Failed,
        "superseded" => RefreshState::Superseded,
        _ => return Err(AppError::Internal.into()),
    };
    snapshot(
        db,
        RefreshReceipt {
            attempt: RefreshAttempt {
                id,
                settings_revision: Revision::try_from(value.0).map_err(|_| AppError::Internal)?,
            },
            state,
            finished_at_ms: value.2,
            total_rows: value
                .3
                .map(|count| count.try_into().map_err(|_| AppError::Internal))
                .transpose()?,
            error: value.4.as_deref().map(parse_failure).transpose()?,
        },
    )
}

fn cursor(value: i64) -> Result<EventCursor, StoreError> {
    EventCursor::try_from(value).map_err(|_| AppError::Internal.into())
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
    let error = error.as_deref().map(parse_failure).transpose()?;
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

fn parse_failure(value: &str) -> Result<SourceFailure, StoreError> {
    match value {
        "unavailable" => Ok(SourceFailure::Unavailable),
        "too_large" => Ok(SourceFailure::TooLarge),
        "interrupted" => Ok(SourceFailure::Interrupted),
        _ => Err(AppError::Internal.into()),
    }
}
