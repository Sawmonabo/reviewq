use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::changes::{DiscussionEpoch, DiscussionEpochStore, DiscussionInvalidation};
use reviewq_app::events::ResourceKey;
use reviewq_domain::identity::{EventCursor, ReviewId, Revision};
use rusqlite::{Connection, OptionalExtension};

use super::{allow_review, identity_for_review, scope, settings_revision};
use crate::connection::WriteTx;
use crate::events::DiscussionObservation;
use crate::{SqliteStore, StoreError};

impl DiscussionEpochStore for SqliteStore {
    fn discussion_epoch(
        &self,
        review: ReviewId,
        settings: Revision,
    ) -> AppFuture<'_, DiscussionEpoch> {
        Box::pin(async move {
            self.read(move |db| epoch(db, &review, settings))
                .await
                .map_err(StoreError::app)
        })
    }

    fn invalidate_discussions(
        &self,
        review: ReviewId,
        settings: Revision,
        expected: EventCursor,
        change: DiscussionInvalidation,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                let current = epoch(&tx.sql, &review, settings)?;
                let denied = change == DiscussionInvalidation::Denied;
                if current.cursor == expected
                    && (change == DiscussionInvalidation::Changed || denied != current.denied)
                {
                    let identity = identity_for_review(&tx.sql, &review)?;
                    allow_review(
                        &tx.sql,
                        scope(&tx.sql, &Actor::local_operator())?,
                        &review,
                        &identity,
                    )?;
                    let denied = if change == DiscussionInvalidation::Changed {
                        current.denied
                    } else {
                        denied
                    };
                    tx.emit_observation(
                        &[ResourceKey::Discussions(Some(review))],
                        None,
                        denied.then_some(settings.get()),
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn epoch(
    db: &Connection,
    review: &ReviewId,
    settings: Revision,
) -> Result<DiscussionEpoch, StoreError> {
    if settings != settings_revision(db)? {
        return Err(AppError::StaleRevision.into());
    }
    // Discussion invalidations own their journal row. The bounded replay window doubles as
    // the committed fence for the short-lived provider projection.
    let latest: Option<(i64, Option<i64>)> = db
        .prepare_cached(
            "SELECT cursor,json_extract(resources,'$.discussion_denied_at') FROM events
         WHERE json_extract(resources,'$.keys[0].kind')='discussions'
         AND json_extract(resources,'$.keys[0].id')=?1 ORDER BY cursor DESC LIMIT 1",
        )?
        .query_row([review.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let (cursor, denied) = latest.unwrap_or_default();
    Ok(DiscussionEpoch {
        cursor: EventCursor::try_from(cursor).map_err(|_| AppError::Internal)?,
        denied: denied == Some(settings.as_sqlite_integer()),
    })
}

pub(crate) fn record(
    tx: &mut WriteTx<'_>,
    review: &ReviewId,
    settings: Revision,
    observed_at: i64,
    fingerprint: [u8; 32],
) -> Result<(), StoreError> {
    let latest: Option<String> = tx
        .sql
        .prepare_cached(
            "SELECT json_extract(resources,'$.discussion') FROM events
         WHERE json_extract(resources,'$.keys[0].kind')='discussions'
           AND json_extract(resources,'$.keys[0].id')=?1
           AND json_type(resources,'$.discussion')='object'
         ORDER BY cursor DESC LIMIT 1",
        )?
        .query_row([review.as_str()], |row| row.get(0))
        .optional()?;
    if let Some(latest) = latest {
        let prior: DiscussionObservation =
            serde_json::from_str(&latest).map_err(|_| AppError::Internal)?;
        if prior.settings == settings.get()
            && (prior.observed_at >= observed_at || prior.fingerprint == fingerprint)
        {
            return Ok(());
        }
    }
    tx.emit_observation(
        &[ResourceKey::Discussions(Some(review.clone()))],
        Some(DiscussionObservation {
            settings: settings.get(),
            observed_at,
            fingerprint,
        }),
        None,
    )?;
    Ok(())
}
