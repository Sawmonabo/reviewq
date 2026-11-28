use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::queue::{
    ManagementFilter, ManagementReviews, ManagementRuns, ReviewLookup, ReviewSource, SelectedLead,
};
use reviewq_app::sources::MergeRequestAddress;
use reviewq_domain::identity::{MrIdentity, ReviewId, Revision, RunId, SessionKey};
use reviewq_domain::session::SessionTarget;
use rusqlite::{Connection, OptionalExtension, params};

use super::{
    allow_review, check_limit, find_review_id, identity_for_review, lead_for_run, management,
    parse, scope, settings_revision, snapshot, source_for_review, text,
};
use crate::refresh::read_sync;
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

    fn known_review(&self, address: MergeRequestAddress) -> AppFuture<'_, Option<ReviewSource>> {
        Box::pin(async move {
            self.read(move |db| known_review(db, &address))
                .await
                .map_err(StoreError::app)
        })
    }

    fn authorized_source(
        &self,
        actor: Actor,
        review: ReviewId,
    ) -> AppFuture<'_, Snapshot<(Revision, ReviewSource)>> {
        Box::pin(async move {
            self.read(move |db| {
                let source = super::source_for_review(db, &review)?;
                allow_review(db, scope(db, &actor)?, &review, &source.identity)?;
                snapshot(db, (settings_revision(db)?, source))
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

fn known_review(
    db: &Connection,
    address: &MergeRequestAddress,
) -> Result<Option<ReviewSource>, StoreError> {
    let sync = read_sync(db, settings_revision(db)?)?;
    if sync.last_success_at_ms.is_none() || sync.error.is_some() {
        return Ok(None);
    }
    let mut statement = db.prepare_cached(
        "SELECT r.review_id, r.state='open'
           AND EXISTS(SELECT 1 FROM review_discoveries d WHERE d.review_id=r.review_id)
           AND EXISTS(SELECT 1 FROM configured_projects q WHERE (q.origin,q.project_id)=(r.origin,r.project_id))
         FROM reviews r WHERE r.origin=?1 AND r.project_path=?2 AND r.iid=?3 LIMIT 2",
    )?;
    let mut rows = statement.query(params![
        address.origin.as_str(),
        address.project_path,
        address.iid.as_sqlite_integer()
    ])?;
    let mut found = Vec::new();
    while let Some(row) = rows.next()? {
        found.push((text(row, 0)?.to_owned(), row.get::<_, bool>(1)?));
    }
    match found.as_slice() {
        [(id, true)] => source_for_review(db, &parse(id)?).map(Some),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use reviewq_app::queue::ReviewLookup;
    use reviewq_app::sources::MergeRequestAddress;
    use reviewq_domain::identity::GitLabOrigin;
    use reviewq_domain::session::AbsolutePath;

    use crate::SqliteStore;

    /// A link uses a stored review only when its path is the one the last successful refresh
    /// read: open, discovered, in a configured project, and the only row at that path and IID.
    #[test]
    fn known_review_needs_a_current_unique_row() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let path = AbsolutePath::from_canonical(temp.path().join("known.sqlite3")).unwrap();
            let store = SqliteStore::open(path).await.unwrap();
            store
                .write(|tx| {
                    tx.sql.execute_batch(
                        "INSERT INTO configured_projects VALUES('https://git.example',1);
                         INSERT INTO source_refresh(singleton,attempt,settings_revision,started_at_ms,last_success_at_ms,active)
                           SELECT 1,1,settings_revision,0,0,0 FROM service_state;
                         INSERT INTO reviews(review_id,origin,project_id,iid,project_path,title,head_sha,state,updated_at_ms)
                           VALUES('current','https://git.example',1,7,'billing/api','Retries',printf('%040d',0),'open',0),
                                 ('foreign','https://git.example',2,7,'billing/old','Old',printf('%040d',0),'open',0),
                                 ('merged','https://git.example',1,6,'billing/api','Merged',printf('%040d',0),'merged',0),
                                 ('tracked','https://git.example',1,5,'billing/api','Tracked',printf('%040d',0),'open',0),
                                 ('renamed','https://git.example',1,8,'billing/api','Renamed',printf('%040d',0),'open',0),
                                 ('reused','https://git.example',3,8,'billing/api','Reused',printf('%040d',0),'open',0);
                         INSERT INTO review_discoveries VALUES('current'),('foreign'),('merged'),('renamed');
                         INSERT INTO review_tracking VALUES('tracked');",
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            let known = async |path: &str, iid: &str| {
                let address = MergeRequestAddress {
                    origin: GitLabOrigin::from_canonical("https://git.example").unwrap(),
                    project_path: path.into(),
                    iid: iid.parse().unwrap(),
                };
                let found = store.known_review(address).await.unwrap();
                found.map(|review| review.identity.iid().get())
            };
            assert_eq!(known("billing/api", "7").await, Some(7));
            assert_eq!(known("billing/old", "7").await, None);
            assert_eq!(known("billing/api", "6").await, None);
            assert_eq!(known("billing/api", "5").await, None);
            assert_eq!(known("billing/api", "8").await, None);
            store
                .write(|tx| {
                    tx.sql
                        .execute("UPDATE source_refresh SET error='unavailable'", [])?;
                    Ok(())
                })
                .await
                .unwrap();
            assert_eq!(known("billing/api", "7").await, None);
            store.close(Duration::from_secs(1)).await.unwrap();
        });
    }
}
