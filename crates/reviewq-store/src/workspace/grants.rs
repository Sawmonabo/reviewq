use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::AppError;
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::workspace::{ConfiguredProject, GrantAddress, GrantBinding, IssueGrant};
use reviewq_domain::identity::{GitLabOrigin, GrantId, Revision};
use reviewq_domain::session::AbsolutePath;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use super::settings_revision;
use crate::StoreError;
use crate::connection::WriteTx;
use crate::events::snapshot;

pub(super) fn exact(
    db: &Connection,
    address: GrantAddress,
) -> Result<Snapshot<GrantBinding>, StoreError> {
    if settings_revision(db)? != address.settings {
        return Err(AppError::StaleRevision.into());
    }
    let lookup=db.prepare_cached(
        "SELECT g.grant_id,g.origin,g.epoch FROM repo_grants g JOIN configured_projects USING(origin,project_id)
         WHERE g.active=1 AND (g.project_id,g.checkout,g.common_dir,g.verifier)=(?1,?2,?3,?4)"
    )?.query_row(
        params![address.project.as_sqlite_integer(),address.checkout.as_path().as_os_str().as_bytes(),
           address.common_dir.as_path().as_os_str().as_bytes(),address.verifier.as_slice()],
       |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?)),
    ).optional()?.ok_or(AppError::NotFound)?;
    snapshot(
        db,
        GrantBinding {
            id: lookup.0.parse().map_err(|_| AppError::Internal)?,
            issuing_project: ConfiguredProject {
                origin: GitLabOrigin::from_canonical(&lookup.1).map_err(|_| AppError::Internal)?,
                project_id: address.project,
            },
            checkout: address.checkout,
            common_dir: address.common_dir,
            epoch: Revision::try_from(lookup.2).map_err(|_| AppError::Internal)?,
            verifier: Some(address.verifier),
        },
    )
}

/// Whether `verifier` belongs to a withdrawn grant and to no active one. Such a key never
/// authenticates again and is never issued again, so a checkout that still holds it needs a
/// fresh key: the service's setup loop and `checkout setup` both ask this before issuing.
pub(crate) fn retired(db: &Connection, verifier: &[u8; 32]) -> Result<bool, StoreError> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM repo_grants WHERE active=0 AND verifier=?1)
            AND NOT EXISTS(SELECT 1 FROM repo_grants WHERE active=1 AND verifier=?1)",
        [verifier.as_slice()],
        |row| row.get(0),
    )?)
}

pub(super) fn issue(
    tx: &mut WriteTx<'_>,
    input: IssueGrant,
) -> Result<Snapshot<GrantBinding>, StoreError> {
    if settings_revision(&tx.sql)? != input.settings {
        return Err(AppError::StaleRevision.into());
    }
    crate::settings::projection::check_issuance(&tx.sql, &input)?;
    if retired(&tx.sql, &input.verifier)? {
        return Err(AppError::Conflict.into());
    }
    let project = &input.project;
    let exists = tx
        .sql
        .query_row(
            "SELECT 1 FROM configured_projects WHERE origin=?1 AND project_id=?2",
            params![
                project.origin.as_str(),
                project.project_id.as_sqlite_integer()
            ],
            |_| Ok(()),
        )
        .optional()?;
    if exists.is_none() {
        return Err(AppError::NotFound.into());
    }
    let checkout = input.checkout.as_path().as_os_str().as_bytes();
    let common = input.common_dir.as_path().as_os_str().as_bytes();
    let matches = tx.sql.prepare_cached(
        "SELECT NOT EXISTS(SELECT 1 FROM settings_repositories WHERE project_id=?1)
              OR EXISTS(SELECT 1 FROM settings_repositories WHERE project_id=?1 AND (checkout,common_dir)=(?2,?3))"
    )?.query_row(params![project.project_id.as_sqlite_integer(),checkout,common],|row|row.get::<_,bool>(0))?;
    if !matches {
        return Err(AppError::Conflict.into());
    }
    let current = tx
        .sql
        .query_row(
            "SELECT grant_id,origin,project_id,checkout,epoch,verifier
         FROM repo_grants WHERE common_dir=?1 AND active=1",
            [common],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                ))
            },
        )
        .optional()?;
    let (id, epoch, changed) =
        match current {
            Some((id, origin, project_id, existing, epoch, verifier)) => {
                if origin != project.origin.as_str()
                    || project_id != project.project_id.as_sqlite_integer()
                    || checkout != existing
                {
                    return Err(AppError::Conflict.into());
                }
                let id: GrantId = id.parse().map_err(|_| AppError::Internal)?;
                let epoch = Revision::try_from(epoch).map_err(|_| AppError::Internal)?;
                if verifier.as_deref() == Some(input.verifier.as_slice()) {
                    (id, epoch, false)
                } else {
                    let epoch = epoch.checked_add(1).ok_or(AppError::Internal)?;
                    tx.sql.execute(
                    "UPDATE repo_grants SET epoch=?2,verifier=?3 WHERE grant_id=?1 AND active=1",
                    params![id.as_str(),epoch.as_sqlite_integer(),input.verifier.as_slice()],
                )?;
                    (id, epoch, true)
                }
            }
            None => insert(tx, project, checkout, common, &input.verifier)?,
        };
    if changed {
        tx.emit(&[ResourceKey::Settings])?;
    }
    snapshot(
        &tx.sql,
        GrantBinding {
            id,
            epoch,
            issuing_project: input.project,
            checkout: input.checkout,
            common_dir: input.common_dir,
            verifier: Some(input.verifier),
        },
    )
}

fn insert(
    tx: &mut WriteTx<'_>,
    project: &ConfiguredProject,
    checkout: &[u8],
    common: &[u8],
    verifier: &[u8; 32],
) -> Result<(GrantId, Revision, bool), StoreError> {
    let id: GrantId = Uuid::new_v4()
        .to_string()
        .parse()
        .map_err(|_| AppError::Internal)?;
    let epoch = Revision::ZERO.checked_add(1).ok_or(AppError::Internal)?;
    tx.sql.execute(
        "INSERT INTO repo_grants(grant_id,origin,project_id,checkout,common_dir,epoch,active,verifier)
         VALUES (?1,?2,?3,?4,?5,?6,1,?7)",
        params![id.as_str(),project.origin.as_str(),project.project_id.as_sqlite_integer(),
            checkout,common,epoch.as_sqlite_integer(),verifier.as_slice()],
    )?;
    Ok((id, epoch, true))
}

pub(super) fn revoke(
    tx: &mut WriteTx<'_>,
    common: AbsolutePath,
) -> Result<Snapshot<bool>, StoreError> {
    let suppressed = crate::settings::projection::suppress(&tx.sql, &common)?;
    let changed = tx.sql.execute(
        "UPDATE repo_grants SET active=0 WHERE common_dir=?1 AND active=1",
        [common.as_path().as_os_str().as_bytes()],
    )? > 0;
    if changed || suppressed {
        tx.projection_changed = true;
        tx.emit(&[ResourceKey::Settings])?;
    }
    snapshot(&tx.sql, changed)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use reviewq_app::access::{Actor, AppError};
    use reviewq_app::workspace::{ConfiguredProject, IssueGrant, WorkspaceStore};
    use reviewq_domain::identity::{GitLabOrigin, ProjectId};
    use reviewq_domain::session::AbsolutePath;

    use crate::SqliteStore;

    /// A checkout whose grant was withdrawn holds a retired key: it may never be issued again,
    /// so setup must replace it. A key some active grant still holds is not retired, even when
    /// an older withdrawn grant had it too, so setup never discards a working checkout's key.
    #[test]
    fn only_a_withdrawn_key_is_retired() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                let temp = tempfile::tempdir().unwrap();
                std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
                let path = temp.path().join("grants.sqlite3");
                let store = SqliteStore::open(AbsolutePath::from_canonical(path).unwrap())
                    .await
                    .unwrap();
                // Checkout a was revoked; checkout b was revoked and its key later restored by
                // a workspace replacement; checkout c is set up.
                let settings = store
                    .write(|tx| {
                        tx.sql.execute_batch(
                            "INSERT INTO configured_projects VALUES('https://git.example',11);
                             INSERT INTO repo_grants VALUES
                              ('a-old','https://git.example',11,CAST('/work/a' AS BLOB),CAST('/work/a' AS BLOB),1,zeroblob(32),0),
                              ('b-old','https://git.example',11,CAST('/work/b' AS BLOB),CAST('/work/b' AS BLOB),1,x'0200000000000000000000000000000000000000000000000000000000000000',0),
                              ('b-new','https://git.example',11,CAST('/work/b' AS BLOB),CAST('/work/b' AS BLOB),2,x'0200000000000000000000000000000000000000000000000000000000000000',1),
                              ('c','https://git.example',11,CAST('/work/c' AS BLOB),CAST('/work/c' AS BLOB),1,x'0300000000000000000000000000000000000000000000000000000000000000',1);",
                        )?;
                        super::settings_revision(&tx.sql)
                    })
                    .await
                    .unwrap();
                let key = |first: u8| {
                    let mut key = [0; 32];
                    key[0] = first;
                    key
                };
                let retired = async |verifier| {
                    let actor = Actor::local_operator();
                    store.retired_grant(actor, verifier).await.unwrap().value
                };
                assert!(retired(key(0)).await);
                assert!(!retired(key(2)).await);
                assert!(!retired(key(3)).await);
                assert!(!retired(key(4)).await);

                let issue = |verifier| IssueGrant {
                    settings,
                    project: ConfiguredProject {
                        origin: GitLabOrigin::from_canonical("https://git.example").unwrap(),
                        project_id: ProjectId::try_from(11_i64).unwrap(),
                    },
                    checkout: AbsolutePath::from_canonical("/work/a".into()).unwrap(),
                    common_dir: AbsolutePath::from_canonical("/work/a".into()).unwrap(),
                    verifier,
                    operator_requested: true,
                };
                let actor = Actor::local_operator();
                let refused = store.issue_grant(actor.clone(), issue(key(0))).await;
                assert!(matches!(refused, Err(AppError::Conflict)));
                // The same checkout gets a grant for a fresh key.
                let issued = store.issue_grant(actor, issue(key(4))).await.unwrap();
                assert_eq!(issued.value.common_dir.as_path().to_str(), Some("/work/a"));
                assert!(!retired(key(4)).await);
                store.close(SqliteStore::CLOSE_DEADLINE).await.unwrap();
            });
    }
}
