//! Transaction-local scope checks and configured checkout authority.

use std::collections::HashSet;
use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::{Actor, ActorScope, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::workspace::{
    ConfiguredProject, GrantAddress, GrantBinding, IssueGrant, WorkspaceConfig, WorkspaceContext,
    WorkspaceStore,
};
use reviewq_domain::identity::{GitLabOrigin, MrIdentity, ProjectId, ReviewId, Revision};
use reviewq_domain::session::{AbsolutePath, HostProvider, NativeIdentity};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::events::snapshot;
use crate::{SqliteStore, StoreError};

mod grants;

pub(crate) enum ReadScope {
    Local,
    Grant,
    Review(ReviewId),
}

pub(crate) fn operator(actor: &Actor) -> Result<(), StoreError> {
    if !matches!(actor.scope(), ActorScope::LocalOperator) {
        return Err(AppError::Forbidden.into());
    }
    Ok(())
}

pub(crate) fn scope(connection: &Connection, actor: &Actor) -> Result<ReadScope, StoreError> {
    match actor.scope() {
        ActorScope::LocalOperator => Ok(ReadScope::Local),
        ActorScope::CheckoutGrant(id) => {
            let epoch = actor.grant_epoch().ok_or(AppError::Unauthorized)?;
            let active = connection
                .query_row(
                    "SELECT 1 FROM repo_grants AS grant_row
                 JOIN configured_projects AS project USING(origin, project_id)
                 WHERE grant_id=?1 AND epoch=?2 AND active=1",
                    params![id.as_str(), epoch.as_sqlite_integer()],
                    |_| Ok(()),
                )
                .optional()?;
            if active.is_none() {
                return Err(AppError::Unauthorized.into());
            }
            Ok(ReadScope::Grant)
        }
        ActorScope::NativeSession(native) => {
            native_review(connection, native).map(ReadScope::Review)
        }
    }
}

fn native_review(connection: &Connection, native: &NativeIdentity) -> Result<ReviewId, StoreError> {
    let review = connection
        .query_row(
            "SELECT runs.review_id FROM sessions JOIN runs USING(run_id)
         WHERE provider=?1 AND profile=?2 AND native_id=?3",
            params![
                provider_label(native.provider()),
                native.profile().as_path().as_os_str().as_bytes(),
                native.native_id().as_str()
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(AppError::NotFound)?;
    review.parse().map_err(|_| AppError::Internal.into())
}

pub(crate) fn configured(
    connection: &Connection,
    identity: &MrIdentity,
) -> Result<bool, StoreError> {
    Ok(connection
        .query_row(
            "SELECT 1 FROM configured_projects WHERE origin=?1 AND project_id=?2",
            params![
                identity.origin().as_str(),
                identity.project_id().as_sqlite_integer()
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub(crate) fn allow_review(
    connection: &Connection,
    scope: ReadScope,
    id: &ReviewId,
    source: &MrIdentity,
) -> Result<(), StoreError> {
    let retained = || {
        Ok::<_, StoreError>(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM retained_reviews WHERE review_id=?1)",
            [id.as_str()],
            |row| row.get::<_, bool>(0),
        )?)
    };
    let discovered = || {
        Ok::<_, StoreError>(connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_discoveries WHERE review_id=?1)",
            [id.as_str()],
            |row| row.get::<_, bool>(0),
        )?)
    };
    let allowed = match scope {
        ReadScope::Local => retained()? || (configured(connection, source)? && discovered()?),
        ReadScope::Grant => configured(connection, source)? && (retained()? || discovered()?),
        ReadScope::Review(own) => &own == id,
    };
    if !allowed {
        return Err(AppError::NotFound.into());
    }
    Ok(())
}

pub(crate) fn actor_key(actor: &Actor) -> [u8; 32] {
    let mut digest = Sha256::new();
    match actor.scope() {
        ActorScope::LocalOperator => digest.update(b"operator"),
        ActorScope::CheckoutGrant(id) => {
            digest.update(b"grant");
            hash_field(&mut digest, id.as_str().as_bytes());
        }
        ActorScope::NativeSession(native) => {
            digest.update(b"native");
            hash_field(&mut digest, provider_label(native.provider()).as_bytes());
            hash_field(
                &mut digest,
                native.profile().as_path().as_os_str().as_bytes(),
            );
            hash_field(&mut digest, native.native_id().as_str().as_bytes());
        }
    }
    digest.finalize().into()
}

pub(crate) fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

pub(crate) fn provider_label(provider: HostProvider) -> &'static str {
    match provider {
        HostProvider::Codex => "codex",
        HostProvider::Claude => "claude",
    }
}

pub(crate) fn settings_revision(connection: &Connection) -> Result<Revision, StoreError> {
    revision_column(
        connection,
        "SELECT settings_revision FROM service_state WHERE singleton=1",
    )
}

pub(crate) fn queue_revision(connection: &Connection) -> Result<Revision, StoreError> {
    revision_column(
        connection,
        "SELECT queue_revision FROM service_state WHERE singleton=1",
    )
}

fn revision_column(connection: &Connection, sql: &str) -> Result<Revision, StoreError> {
    let value = connection.query_row(sql, [], |row| row.get::<_, i64>(0))?;
    Revision::try_from(value).map_err(|_| AppError::Internal.into())
}

pub(crate) fn advance_queue(connection: &Connection) -> Result<(), StoreError> {
    let next = queue_revision(connection)?
        .checked_add(1)
        .ok_or(AppError::Internal)?;
    connection.execute(
        "UPDATE service_state SET queue_revision=?1 WHERE singleton=1",
        [next.as_sqlite_integer()],
    )?;
    Ok(())
}

impl WorkspaceStore for SqliteStore {
    fn granted(
        &self,
        actor: Actor,
        address: reviewq_app::workspace::GrantAddress,
    ) -> AppFuture<'_, Snapshot<GrantBinding>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(move |db| grants::exact(db, address))
                .await
                .map_err(StoreError::app)
        })
    }
    fn confirm_grant_projection(
        &self,
        actor: Actor,
        expected: GrantAddress,
    ) -> AppFuture<'_, Snapshot<GrantBinding>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.write(move |tx| {
                let settings = expected.settings;
                let verifier = expected.verifier;
                let result = grants::exact(&tx.sql, expected)?;
                crate::settings::projection::applied_address(
                    tx,
                    reviewq_app::workspace::CheckoutProjection {
                        settings,
                        desired: true,
                        project: result.value.issuing_project.clone(),
                        checkout: reviewq_domain::settings::CheckoutBinding {
                            root: result.value.checkout.clone(),
                            common_dir: result.value.common_dir.clone(),
                        },
                    },
                    verifier,
                )?;
                snapshot(&tx.sql, result.value)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn issue_grant(
        &self,
        actor: Actor,
        input: IssueGrant,
    ) -> AppFuture<'_, Snapshot<GrantBinding>> {
        Box::pin(async move {
            self.write(move |tx| {
                operator(&actor)?;
                grants::issue(tx, input)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn revoke_grant(
        &self,
        actor: Actor,
        common_dir: AbsolutePath,
    ) -> AppFuture<'_, Snapshot<bool>> {
        Box::pin(async move {
            self.write(move |tx| {
                operator(&actor)?;
                grants::revoke(tx, common_dir)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn authenticate_grant(&self, verifier: [u8; 32]) -> AppFuture<'_, Actor> {
        Box::pin(async move {
            self.read(move |tx| {
                let matched = tx
                    .query_row(
                        "SELECT g.grant_id,g.epoch,g.verifier FROM repo_grants g
                     JOIN configured_projects p USING (origin,project_id)
                     WHERE g.active=1 AND g.verifier=?1",
                        [verifier.as_slice()],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, Vec<u8>>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((id, epoch, expected)) = matched else {
                    return Err(AppError::Unauthorized.into());
                };
                if !bool::from(verifier.as_slice().ct_eq(expected.as_slice())) {
                    return Err(AppError::Unauthorized.into());
                }
                let id = id.parse().map_err(|_| AppError::Internal)?;
                let epoch = Revision::try_from(epoch).map_err(|_| AppError::Internal)?;
                Ok(Actor::checkout_grant(id, epoch))
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn workspace_context(&self, actor: Actor) -> AppFuture<'_, Snapshot<WorkspaceContext>> {
        Box::pin(async move {
            self.read(move |tx| {
                if matches!(scope(tx, &actor)?, ReadScope::Review(_)) {
                    return Err(AppError::Forbidden.into());
                }
                snapshot(tx, context(tx)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn replace_workspace(
        &self,
        actor: Actor,
        expected: Revision,
        input: WorkspaceConfig,
    ) -> AppFuture<'_, Snapshot<WorkspaceContext>> {
        Box::pin(async move {
            self.write(move |tx| {
                operator(&actor)?;
                if settings_revision(&tx.sql)? != expected {
                    return Err(AppError::Conflict.into());
                }
                replace(&tx.sql, input)?;
                let next = expected.checked_add(1).ok_or(AppError::Internal)?;
                tx.sql.execute(
                    "UPDATE service_state SET settings_revision=?1 WHERE singleton=1",
                    [next.as_sqlite_integer()],
                )?;
                advance_queue(&tx.sql)?;
                tx.emit(&[ResourceKey::Settings, ResourceKey::Queue])?;
                snapshot(&tx.sql, context(&tx.sql)?)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn context(connection: &Connection) -> Result<WorkspaceContext, StoreError> {
    let mut statement = connection.prepare_cached(
        "SELECT origin, project_id FROM configured_projects ORDER BY origin, project_id",
    )?;
    let mut rows = statement.query([])?;
    let mut projects = Vec::new();
    while let Some(row) = rows.next()? {
        let origin =
            GitLabOrigin::from_canonical(row.get_ref(0)?.as_str().map_err(|_| AppError::Internal)?)
                .map_err(|_| AppError::Internal)?;
        let project_id =
            ProjectId::try_from(row.get::<_, i64>(1)?).map_err(|_| AppError::Internal)?;
        projects.push(ConfiguredProject { origin, project_id });
    }
    Ok(WorkspaceContext {
        revision: settings_revision(connection)?,
        projects,
    })
}

fn replace(connection: &Connection, input: WorkspaceConfig) -> Result<(), StoreError> {
    let projects: HashSet<_> = input.projects.iter().collect();
    let mut ids = HashSet::new();
    let mut dirs = HashSet::new();
    if projects.len() != input.projects.len()
        || input.grants.iter().any(|grant| {
            grant.epoch == Revision::ZERO
                || !ids.insert(&grant.id)
                || !dirs.insert(&grant.common_dir)
                || !projects.contains(&grant.issuing_project)
        })
    {
        return Err(AppError::InvalidRequest.into());
    }
    for grant in &input.grants {
        verify_binding(connection, grant)?;
    }

    connection.execute("DELETE FROM configured_projects", [])?;
    for project in &input.projects {
        connection.execute(
            "INSERT INTO configured_projects(origin, project_id) VALUES(?1, ?2)",
            params![
                project.origin.as_str(),
                project.project_id.as_sqlite_integer()
            ],
        )?;
    }
    connection.execute("UPDATE repo_grants SET active=0 WHERE active=1", [])?;
    for grant in &input.grants {
        connection.execute(
            "INSERT INTO repo_grants(grant_id, origin, project_id, checkout, common_dir, epoch, active, verifier)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)
             ON CONFLICT(grant_id) DO UPDATE SET epoch=excluded.epoch, active=1, verifier=excluded.verifier",
            params![
                grant.id.as_str(), grant.issuing_project.origin.as_str(),
                grant.issuing_project.project_id.as_sqlite_integer(),
                grant.checkout.as_path().as_os_str().as_bytes(), grant.common_dir.as_path().as_os_str().as_bytes(),
                grant.epoch.as_sqlite_integer(),
                grant.verifier.as_ref().map(<[u8; 32]>::as_slice),
            ],
        )?;
    }
    Ok(())
}

type StoredGrant = (String, i64, Vec<u8>, Vec<u8>, i64, bool, Option<Vec<u8>>);

fn verify_binding(connection: &Connection, grant: &GrantBinding) -> Result<(), StoreError> {
    let old: Option<StoredGrant> = connection.query_row(
        "SELECT origin, project_id, checkout, common_dir, epoch, active, verifier FROM repo_grants WHERE grant_id=?1",
        [grant.id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).optional()?;
    if let Some((origin, project, checkout, common_dir, epoch, active, verifier)) = old {
        let requested_epoch = grant.epoch.as_sqlite_integer();
        if origin != grant.issuing_project.origin.as_str()
            || project != grant.issuing_project.project_id.as_sqlite_integer()
            || checkout != grant.checkout.as_path().as_os_str().as_bytes()
            || common_dir != grant.common_dir.as_path().as_os_str().as_bytes()
            || requested_epoch < epoch
            || requested_epoch == epoch
                && (!active
                    || verifier.as_deref() != grant.verifier.as_ref().map(<[u8; 32]>::as_slice))
        {
            return Err(AppError::Conflict.into());
        }
    }
    Ok(())
}
