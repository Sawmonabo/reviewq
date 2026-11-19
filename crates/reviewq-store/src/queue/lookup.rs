use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::queue::{
    ManagementFilter, ManagementReviews, ManagementRuns, ReviewLookup, SelectedLead,
};
use reviewq_domain::identity::{MrIdentity, ReviewId, Revision, RunId, SessionKey};
use reviewq_domain::session::SessionTarget;
use rusqlite::{OptionalExtension, params};

use super::{
    allow_review, check_limit, find_review_id, identity_for_review, lead_for_run, management,
    parse, scope, settings_revision, snapshot,
};
use crate::{SqliteStore, StoreError};

impl ReviewLookup for SqliteStore {
    fn management_reviews(
        &self,
        actor: Actor,
        filter: ManagementFilter,
        offset: u32,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ManagementReviews>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| {
                snapshot(tx, management::reviews(tx, actor, filter, offset, limit)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn management_runs(
        &self,
        actor: Actor,
        review: ReviewId,
        offset: u32,
        limit: u16,
    ) -> AppFuture<'_, Snapshot<ManagementRuns>> {
        Box::pin(async move {
            check_limit(limit)?;
            self.read(move |tx| snapshot(tx, management::runs(tx, actor, review, offset, limit)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn review_by_mr(&self, actor: Actor, mr: MrIdentity) -> AppFuture<'_, ReviewId> {
        Box::pin(async move {
            self.read(move |connection| {
                let access = scope(connection, &actor)?;
                let id = find_review_id(connection, &mr)?.ok_or(AppError::NotFound)?;
                allow_review(connection, access, &id, &mr)?;
                Ok(id)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn authorized_source(
        &self,
        actor: Actor,
        review: ReviewId,
    ) -> AppFuture<'_, Snapshot<(Revision, MrIdentity)>> {
        Box::pin(async move {
            self.read(move |db| {
                let identity = identity_for_review(db, &review)?;
                allow_review(db, scope(db, &actor)?, &review, &identity)?;
                snapshot(db, (settings_revision(db)?, identity))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn registered_session(
        &self,
        actor: Actor,
        session: SessionKey,
    ) -> AppFuture<'_, Option<SessionTarget>> {
        Box::pin(async move {
            self.read(move |db| {
                let binding = db
                    .prepare_cached(
                        "SELECT r.review_id, r.run_id FROM sessions s JOIN runs r USING(run_id)
                     WHERE s.session_key=?1",
                    )?
                    .query_row([session.as_str()], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .optional()?;
                let Some((review, run)) = binding else {
                    return Ok(None);
                };
                let review = parse(&review)?;
                let mr = identity_for_review(db, &review)?;
                allow_review(db, scope(db, &actor)?, &review, &mr)?;
                Ok(Some(SessionTarget::new(review, parse(&run)?, session)))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn run_for_session(
        &self,
        actor: Actor,
        review: ReviewId,
        session: SessionKey,
    ) -> AppFuture<'_, RunId> {
        Box::pin(async move {
            self.read(move |connection| {
                let access = scope(connection, &actor)?;
                let mr = identity_for_review(connection, &review)?;
                allow_review(connection, access, &review, &mr)?;
                let id = connection.query_row(
                    "SELECT r.run_id FROM runs r JOIN sessions s USING(run_id) WHERE r.review_id=?1 AND s.session_key=?2",
                    params![review.as_str(), session.as_str()],
                    |row| row.get::<_, String>(0),
                ).optional()?.ok_or(AppError::NotFound)?;
                parse(&id)
            }).await.map_err(StoreError::app)
        })
    }

    fn resolve_selection(
        &self,
        actor: Actor,
        review_id: ReviewId,
        run_id: Option<RunId>,
    ) -> AppFuture<'_, Snapshot<SelectedLead>> {
        Box::pin(async move {
            self.read(move |tx| {
                let access = scope(tx, &actor)?;
                let identity = identity_for_review(tx, &review_id)?;
                allow_review(tx, access, &review_id, &identity)?;
                let chosen = match run_id {
                    Some(id) => {
                        let lead = lead_for_run(tx, &id)?.ok_or(AppError::NotFound)?;
                        if lead.review_id() != &review_id {
                            return Err(AppError::NotFound.into());
                        }
                        Some(lead)
                    }
                    None => {
                        let latest = tx.query_row(
                            "SELECT run_id FROM runs WHERE review_id=?1 ORDER BY started_at_ms DESC, run_id DESC LIMIT 1",
                            [review_id.as_str()], |row| row.get::<_, String>(0),
                        ).optional()?;
                        latest.map(|id| lead_for_run(tx, &parse(&id)?)).transpose()?.flatten()
                    }
                };
                snapshot(tx, SelectedLead::new(review_id, chosen)?)
            }).await.map_err(StoreError::app)
        })
    }
}
