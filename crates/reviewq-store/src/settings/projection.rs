use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::events::ResourceKey;
use reviewq_app::settings::ChatSetup;
use reviewq_app::workspace::{
    CheckoutProjection, ConfiguredProject, IssueGrant, ProjectionOutcome,
    RepositoryChatProjectionStore,
};
use reviewq_domain::identity::{GitLabOrigin, ProjectId};
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::CheckoutBinding;
use rusqlite::{Connection, OptionalExtension, params};

use crate::connection::WriteTx;
use crate::queue::path;
use crate::workspace::settings_revision;
use crate::{SqliteStore, StoreError};

impl RepositoryChatProjectionStore for SqliteStore {
    fn checkout_projections(
        &self,
        include_applied: bool,
        after: Option<AbsolutePath>,
    ) -> AppFuture<'_, Vec<CheckoutProjection>> {
        Box::pin(async move {
            self.read(move |db| {
                let settings = settings_revision(db)?;
                let mut rows = db.prepare_cached(
                    "SELECT common_dir,origin,project_id,checkout,desired FROM checkout_projections
                     WHERE suppressed=0 AND (?1 OR ready=0) AND (?2 IS NULL OR common_dir>?2)
                     ORDER BY common_dir LIMIT 16",
                )?;
                let mut rows = rows.query(params![include_applied, after.as_ref().map(bytes)])?;
                let mut entries = Vec::new();
                while let Some(row) = rows.next()? {
                    let origin = row.get::<_, String>(1)?;
                    entries.push(CheckoutProjection {
                        settings,
                        project: ConfiguredProject {
                            origin: GitLabOrigin::from_canonical(&origin)
                                .map_err(|_| AppError::Internal)?,
                            project_id: ProjectId::try_from(row.get::<_, i64>(2)?)
                                .map_err(|_| AppError::Internal)?,
                        },
                        checkout: CheckoutBinding {
                            root: path(row.get(3)?)?,
                            common_dir: path(row.get(0)?)?,
                        },
                        desired: row.get(4)?,
                    });
                }
                Ok(entries)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn check_checkout_projection(&self, job: CheckoutProjection) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.read(move |db| check(db, &job))
                .await
                .map_err(StoreError::app)
        })
    }

    fn retired_checkout_verifier(
        &self,
        job: CheckoutProjection,
        verifier: [u8; 32],
    ) -> AppFuture<'_, bool> {
        Box::pin(async move {
            self.read(move |db| {
                check(db, &job)?;
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM repo_grants WHERE active=0 AND common_dir=?1 AND verifier=?2)",
                    params![bytes(&job.checkout.common_dir), verifier.as_slice()], |row| row.get(0),
                )?)
            }).await.map_err(StoreError::app)
        })
    }

    fn finish_checkout_projection(
        &self,
        job: CheckoutProjection,
        outcome: ProjectionOutcome,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            self.write(move |tx| {
                check(&tx.sql, &job)?;
                let common = bytes(&job.checkout.common_dir);
                let changed = match outcome {
                    ProjectionOutcome::Applied(verifier) if job.desired => {
                        applied(tx, &job, &verifier)?
                    }
                    ProjectionOutcome::Removed if !job.desired => tx.sql.execute(
                        "DELETE FROM checkout_projections WHERE common_dir=?1 AND desired=0",
                        [common],
                    )?,
                    ProjectionOutcome::Deferred(issue)
                        if !issue.is_empty() && issue.len() <= 1024 =>
                    {
                        tx.sql.execute(
                            "UPDATE checkout_projections SET issue=?2,ready=0
                         WHERE common_dir=?1 AND (issue IS DISTINCT FROM ?2 OR ready<>0)",
                            params![common, issue],
                        )?
                    }
                    _ => return Err(AppError::InvalidRequest.into()),
                };
                if changed != 0 {
                    tx.emit(&[ResourceKey::Settings])?;
                }
                Ok(())
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

pub(super) fn set_desired(db: &Connection) -> Result<(), StoreError> {
    db.execute_batch(
        "UPDATE checkout_projections AS c SET desired=0,suppressed=0,ready=0,issue=NULL WHERE desired=1 AND
           NOT EXISTS (SELECT 1 FROM settings_repositories r JOIN configured_projects p ON p.project_id=r.project_id
                       WHERE (r.common_dir,r.checkout,r.project_id,p.origin)=(c.common_dir,c.checkout,c.project_id,c.origin));
         INSERT INTO checkout_projections(common_dir,origin,project_id,checkout,desired,suppressed,ready)
         SELECT r.common_dir,p.origin,r.project_id,r.checkout,1,0,0
         FROM settings_repositories r JOIN configured_projects p ON p.project_id=r.project_id WHERE r.common_dir IS NOT NULL
         ON CONFLICT(common_dir) DO UPDATE SET origin=excluded.origin,project_id=excluded.project_id,
            checkout=excluded.checkout,desired=1,suppressed=0,ready=0,issue=NULL
         WHERE (origin,project_id,checkout,desired) IS DISTINCT FROM
           (excluded.origin,excluded.project_id,excluded.checkout,excluded.desired);",
    )?;
    Ok(())
}

pub(super) fn summary(db: &Connection) -> Result<ChatSetup, StoreError> {
    let (pending, cleanup_conflict) = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM checkout_projections WHERE ready=0),
                EXISTS(SELECT 1 FROM checkout_projections WHERE desired=0 AND issue IS NOT NULL)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let issues = db
        .prepare_cached(
            "SELECT issue FROM checkout_projections WHERE issue IS NOT NULL
         ORDER BY desired, common_dir LIMIT 32",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ChatSetup {
        pending,
        cleanup_conflict,
        issues,
    })
}

pub(crate) fn check_issuance(db: &Connection, input: &IssueGrant) -> Result<(), StoreError> {
    let suppression = db
        .query_row(
            "SELECT suppressed FROM checkout_projections WHERE common_dir=?1 AND desired=1
         AND (origin,project_id,checkout)=(?2,?3,?4)",
            params![
                bytes(&input.common_dir),
                input.project.origin.as_str(),
                input.project.project_id.as_sqlite_integer(),
                bytes(&input.checkout)
            ],
            |row| row.get::<_, bool>(0),
        )
        .optional()?;
    if !input.operator_requested && suppression != Some(false) {
        return Err(AppError::StaleRevision.into());
    }
    if input.operator_requested && suppression == Some(true) {
        db.execute(
            "UPDATE checkout_projections SET suppressed=0,issue=NULL WHERE common_dir=?1",
            [bytes(&input.common_dir)],
        )?;
    }
    Ok(())
}

pub(crate) fn suppress(db: &Connection, common: &AbsolutePath) -> Result<bool, StoreError> {
    Ok(db.execute(
        "UPDATE checkout_projections SET suppressed=1,ready=0,
         issue='Management access was revoked. Run checkout setup to restore access.'
         WHERE common_dir=?1 AND desired=1 AND suppressed=0",
        [bytes(common)],
    )? != 0)
}

pub(crate) fn applied_address(
    tx: &mut WriteTx<'_>,
    job: CheckoutProjection,
    verifier: [u8; 32],
) -> Result<(), StoreError> {
    let current: bool = tx.sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM checkout_projections WHERE common_dir=?1 AND desired=1)",
        [bytes(&job.checkout.common_dir)],
        |row| row.get(0),
    )?;
    if current {
        check(&tx.sql, &job)?;
        if applied(tx, &job, &verifier)? != 0 {
            tx.emit(&[ResourceKey::Settings])?;
        }
    }
    Ok(())
}

fn applied(
    tx: &mut WriteTx<'_>,
    job: &CheckoutProjection,
    verifier: &[u8; 32],
) -> Result<usize, StoreError> {
    let db = &tx.sql;
    let common = bytes(&job.checkout.common_dir);
    let exact: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM repo_grants WHERE active=1 AND
          (common_dir,origin,project_id,checkout,verifier)=(?1,?2,?3,?4,?5))",
        params![
            common,
            job.project.origin.as_str(),
            job.project.project_id.as_sqlite_integer(),
            bytes(&job.checkout.root),
            verifier.as_slice()
        ],
        |row| row.get(0),
    )?;
    if !exact {
        return Err(AppError::StaleRevision.into());
    }
    Ok(db.execute(
        "UPDATE checkout_projections SET issue=NULL,ready=1 WHERE common_dir=?1 AND ready=0",
        [common],
    )?)
}

fn check(db: &Connection, job: &CheckoutProjection) -> Result<(), StoreError> {
    if settings_revision(db)? != job.settings
        || !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM checkout_projections WHERE
          (common_dir,origin,project_id,checkout,desired,suppressed)=(?1,?2,?3,?4,?5,0))",
            params![
                bytes(&job.checkout.common_dir),
                job.project.origin.as_str(),
                job.project.project_id.as_sqlite_integer(),
                bytes(&job.checkout.root),
                job.desired
            ],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Err(AppError::StaleRevision.into());
    }
    Ok(())
}

fn bytes(path: &AbsolutePath) -> &[u8] {
    path.as_path().as_os_str().as_bytes()
}
