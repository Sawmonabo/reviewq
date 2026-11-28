use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::Snapshot;
use reviewq_app::queue::{
    ManagementFilter, ManagementReviews, ManagementRuns, ReviewLookup, ReviewSource, SelectedLead,
};
use reviewq_app::sources::ReviewAddress;
use reviewq_domain::identity::{MrIdentity, ReviewId, Revision, RunId, SessionKey};
use reviewq_domain::session::SessionTarget;
use rusqlite::{Connection, OptionalExtension, params};

use super::workspace::read_scope;
use super::{
    SCOPE_PREDICATE, allow_review, check_limit, find_review_id, identity_for_review, lead_for_run,
    management, parse, scope, settings_revision, snapshot, source_for_review, text,
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

    fn known_review(
        &self,
        actor: Actor,
        address: ReviewAddress,
    ) -> AppFuture<'_, Option<ReviewSource>> {
        Box::pin(async move {
            self.read(move |db| known_review(db, &actor, &address))
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
    actor: &Actor,
    address: &ReviewAddress,
) -> Result<Option<ReviewSource>, StoreError> {
    let access = scope(db, actor)?;
    let (code, owner) = read_scope(&access);
    let mut found = match address {
        ReviewAddress::MergeRequest {
            origin,
            project_path,
            iid,
        } => candidates(
            db,
            "r.project_path=?4 AND r.iid=?5",
            params![
                code,
                owner,
                origin.as_str(),
                project_path,
                iid.as_sqlite_integer()
            ],
        )?,
        ReviewAddress::Ticket { origin, key } => candidates(
            db,
            "r.state='open' AND r.review_id IN (
               SELECT review_id FROM scoped_review_issues WHERE ticket_key=?4
               UNION SELECT review_id FROM retained_review_tickets WHERE ticket_key=?4)",
            params![code, owner, origin.as_str(), key.as_str()],
        )?,
    };
    Ok(if found.len() == 1 { found.pop() } else { None })
}

/// Reads at most two visible reviews in configured projects that match `filter`, enough to
/// tell one from several. Parameters 1 and 2 are the read scope, 3 the origin, and 4 onward
/// the filter's.
fn candidates(
    db: &Connection,
    filter: &str,
    values: &[&dyn rusqlite::ToSql],
) -> Result<Vec<ReviewSource>, StoreError> {
    let sql = format!(
        "SELECT r.review_id FROM reviews r WHERE {SCOPE_PREDICATE} AND r.origin=?3 AND {filter}
           AND EXISTS(SELECT 1 FROM configured_projects q WHERE (q.origin,q.project_id)=(r.origin,r.project_id))
         LIMIT 2"
    );
    let mut statement = db.prepare_cached(&sql)?;
    let mut rows = statement.query(values)?;
    let mut found = Vec::new();
    while let Some(row) = rows.next()? {
        found.push(source_for_review(db, &parse(text(row, 0)?)?)?);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use reviewq_app::access::Actor;
    use reviewq_app::queue::ReviewLookup;
    use reviewq_app::sources::ReviewAddress;
    use reviewq_domain::identity::GitLabOrigin;
    use reviewq_domain::session::AbsolutePath;

    use crate::SqliteStore;

    /// A link reads a stored review only in a configured project, and for a ticket only its
    /// one open review. A tracked review whose project left the settings stays visible to the
    /// operator, yet is left to the source, as is a ticket with only a merged review.
    #[test]
    fn known_review_needs_a_configured_project_and_an_open_ticket_review() {
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
                         INSERT INTO reviews(review_id,origin,project_id,iid,project_path,title,head_sha,state,updated_at_ms)
                           VALUES('open','https://git.example',1,7,'billing/api','Retries',printf('%040d',0),'open',0),
                                 ('merged','https://git.example',1,6,'billing/api','First',printf('%040d',0),'merged',0),
                                 ('removed','https://git.example',2,7,'billing/old','Old',printf('%040d',0),'open',0);
                         INSERT INTO review_discoveries VALUES('open'),('merged');
                         INSERT INTO review_tracking VALUES('removed');
                         INSERT INTO retained_review_tickets VALUES('open','RQ-1','Retries',0),
                           ('merged','RQ-1','First',0),('merged','RQ-3','First',0),
                           ('removed','RQ-2','Old',0);",
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            let origin = || GitLabOrigin::from_canonical("https://git.example").unwrap();
            let known = async |address| {
                let found = store.known_review(Actor::local_operator(), address).await;
                found.unwrap().map(|review| review.identity.iid().get())
            };
            let merge_request = |path: &str| ReviewAddress::MergeRequest {
                origin: origin(),
                project_path: path.into(),
                iid: "7".parse().unwrap(),
            };
            let ticket = |key: &str| ReviewAddress::Ticket {
                origin: origin(),
                key: key.parse().unwrap(),
            };
            assert_eq!(known(merge_request("billing/api")).await, Some(7));
            assert_eq!(known(ticket("RQ-1")).await, Some(7));
            assert_eq!(known(ticket("RQ-3")).await, None);
            assert_eq!(known(merge_request("billing/old")).await, None);
            assert_eq!(known(ticket("RQ-2")).await, None);
            store.close(Duration::from_secs(1)).await.unwrap();
        });
    }
}
