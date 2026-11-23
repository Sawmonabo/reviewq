//! Settings projection and private credential reads under the same revision fence.

use std::collections::HashSet;

use reviewq_app::access::{Actor, AppError, AppFuture};
use reviewq_app::events::{ResourceKey, Snapshot};
use reviewq_app::queue::ReviewAuthor;
use reviewq_app::settings::{
    ConnectionSummary, Connections, ReducedWorkspace, SavedConnection, SavedCredentials,
    SecretToken, SettingsStore,
};
use reviewq_domain::identity::Revision;
use reviewq_domain::settings::{
    Connection, Identity, JiraTokenType, Provider, VerifiedAccount, WorkspaceScope,
};
use rusqlite::{Connection as Database, OptionalExtension, params};

use crate::connection::WriteTx;
use crate::events::snapshot;
use crate::workspace::{advance_queue, operator, settings_revision};
use crate::{SqliteStore, StoreError};

pub(crate) mod projection;
mod scope;

const REVIEWER_SUGGESTIONS: usize = 20;
const RECENT_REVIEWS: u32 = 256;

impl SettingsStore for SqliteStore {
    fn preference_revision(&self) -> AppFuture<'_, Revision> {
        Box::pin(async move {
            self.read(|db| settings_revision(db))
                .await
                .map_err(StoreError::app)
        })
    }

    fn read_credentials(&self, actor: Actor) -> AppFuture<'_, Snapshot<SavedCredentials>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(|db| {
                snapshot(
                    db,
                    SavedCredentials {
                        revision: settings_revision(db)?,
                        pinned: pinned(db)?,
                        gitlab: saved(db, Provider::GitLab)?,
                        jira: saved(db, Provider::Jira)?,
                        scope: scope::read(db)?,
                    },
                )
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn settings(&self, actor: Actor) -> AppFuture<'_, Snapshot<Connections>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(|db| snapshot(db, view(db)?))
                .await
                .map_err(StoreError::app)
        })
    }

    fn reviewer_suggestions(
        &self,
        actor: Actor,
        expected: Revision,
        origin: String,
    ) -> AppFuture<'_, Vec<ReviewAuthor>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.read(move |db| {
                recheck(db, expected)?;
                let mut statement = db.prepare_cached(
                    "SELECT r.author_id, r.author_username
                     FROM reviews r JOIN configured_projects p USING (origin, project_id)
                     JOIN settings s ON s.provider='gitlab' AND s.origin=r.origin
                     JOIN credentials c ON c.provider=s.provider
                     WHERE r.origin=?1 AND r.author_id<>CAST(s.identity_id AS INTEGER)
                       AND NOT EXISTS (SELECT 1 FROM settings_reviewers u WHERE u.reviewer_id=r.author_id)
                       AND (EXISTS (SELECT 1 FROM review_discoveries d WHERE d.review_id=r.review_id)
                            OR EXISTS (SELECT 1 FROM retained_reviews v WHERE v.review_id=r.review_id))
                     ORDER BY r.updated_at_ms DESC,r.review_id DESC LIMIT ?2",
                )?;
                let mut rows = statement.query(params![origin, RECENT_REVIEWS])?;
                let mut seen = HashSet::new();
                let mut authors = Vec::new();
                while authors.len() < REVIEWER_SUGGESTIONS {
                    let Some(row) = rows.next()? else { break };
                    let id = u64::try_from(row.get::<_, i64>(0)?).map_err(|_| AppError::Internal)?;
                    if seen.insert(id) {
                        authors.push(ReviewAuthor { id, username: row.get(1)? });
                    }
                }
                Ok(authors)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn activate_connections(
        &self,
        actor: Actor,
        expected: Revision,
        gitlab: SavedConnection,
        jira: SavedConnection,
        scope: WorkspaceScope,
    ) -> AppFuture<'_, Snapshot<Connections>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.write(move |tx| {
                recheck(&tx.sql, expected)?;
                if gitlab.account.connection.provider() != Provider::GitLab
                    || jira.account.connection.provider() != Provider::Jira
                {
                    return Err(AppError::InvalidRequest.into());
                }
                let origin = gitlab.account.connection.origin().to_owned();
                let pinned = pinned(&tx.sql)?;
                let mut changed = scope::discovery_changed(&tx.sql, &scope)?;
                for connection in [gitlab, jira] {
                    let provider = connection.account.connection.provider();
                    let prior = account(&tx.sql, provider)?;
                    let same = prior
                        .as_ref()
                        .is_some_and(|prior| prior.same_authority(&connection.account));
                    if pinned && prior.is_some() && !same {
                        return Err(AppError::Conflict.into());
                    }
                    let secret = connection.token.as_ref().ok_or(AppError::InvalidRequest)?;
                    let same_access = same && tx.sql.query_row(
                        "SELECT EXISTS(SELECT 1 FROM credentials WHERE provider=?1 AND token=?2)",
                        params![label(provider), secret.expose()],
                        |row| row.get::<_, bool>(0),
                    )?;
                    changed |= !same_access;
                    if provider == Provider::GitLab && !same_access {
                        invalidate_history(&tx.sql)?;
                    }
                    save(&tx.sql, connection)?;
                }
                commit_scope(tx, expected, &origin, scope, changed)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn reduce_workspace(
        &self,
        actor: Actor,
        expected: Revision,
        reduced: ReducedWorkspace,
    ) -> AppFuture<'_, Snapshot<Connections>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.write(move |tx| {
                recheck(&tx.sql, expected)?;
                let gitlab =
                    account(&tx.sql, Provider::GitLab)?.ok_or(AppError::PreconditionFailed)?;
                let scope = reduced.into_scope();
                let changed = scope::discovery_changed(&tx.sql, &scope)?;
                commit_scope(tx, expected, gitlab.connection.origin(), scope, changed)
            })
            .await
            .map_err(StoreError::app)
        })
    }

    fn disconnect(
        &self,
        actor: Actor,
        expected: Revision,
        provider: Provider,
    ) -> AppFuture<'_, Snapshot<Connections>> {
        Box::pin(async move {
            operator(&actor).map_err(StoreError::app)?;
            self.write(move |tx| {
                recheck(&tx.sql, expected)?;
                if tx.sql.execute(
                    "DELETE FROM credentials WHERE provider=?1",
                    [label(provider)],
                )? == 0
                {
                    return snapshot(&tx.sql, view(&tx.sql)?);
                }
                if provider == Provider::GitLab {
                    invalidate_history(&tx.sql)?;
                }
                advance(tx, expected)
            })
            .await
            .map_err(StoreError::app)
        })
    }
}

fn invalidate_history(db: &Database) -> Result<(), StoreError> {
    db.execute(
        "UPDATE source_history_projects SET valid=0 WHERE valid<>0",
        [],
    )?;
    Ok(())
}

fn commit_scope(
    tx: &mut WriteTx<'_>,
    expected: Revision,
    origin: &str,
    scope: WorkspaceScope,
    changed: bool,
) -> Result<Snapshot<Connections>, StoreError> {
    if changed {
        crate::queue::tracking::retain_current_tickets(&tx.sql, None)?;
        tx.sql.execute_batch("DELETE FROM review_discoveries; DELETE FROM scoped_issues; DELETE FROM scoped_sprints;")?;
        tx.emit(&[ResourceKey::Ticket(None)])?;
    }
    scope::replace(&tx.sql, origin, scope)?;
    advance(tx, expected)
}

fn account(db: &Database, provider: Provider) -> Result<Option<VerifiedAccount>, StoreError> {
    let mut query = db.prepare_cached("SELECT origin,email,token_type,cloud_id,identity_id,identity_label FROM settings WHERE provider=?1")?;
    let mut rows = query.query([label(provider)])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let origin = row.get(0)?;
    let connection = match provider {
        Provider::GitLab => Connection::GitLab { origin },
        Provider::Jira => Connection::Jira {
            site: origin,
            email: row.get(1)?,
            token_type: match row.get::<_, String>(2)?.as_str() {
                "scoped" => JiraTokenType::Scoped,
                "unscoped" => JiraTokenType::Unscoped,
                _ => return Err(AppError::Internal.into()),
            },
        },
    };
    Ok(Some(VerifiedAccount {
        connection,
        cloud_id: row.get(3)?,
        identity: Identity {
            id: row.get(4)?,
            label: row.get(5)?,
        },
    }))
}

fn saved(db: &Database, provider: Provider) -> Result<Option<SavedConnection>, StoreError> {
    let Some(account) = account(db, provider)? else {
        return Ok(None);
    };
    let token = db
        .query_row(
            "SELECT token FROM credentials WHERE provider=?1",
            [label(provider)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(SecretToken::new)
        .transpose()
        .map_err(|_| AppError::Internal)?;
    Ok(Some(SavedConnection { account, token }))
}

fn summary(db: &Database, provider: Provider) -> Result<Option<ConnectionSummary>, StoreError> {
    let Some(account) = account(db, provider)? else {
        return Ok(None);
    };
    let configured = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM credentials WHERE provider=?1)",
        [label(provider)],
        |row| row.get(0),
    )?;
    Ok(Some(ConnectionSummary {
        account,
        configured,
    }))
}

fn view(db: &Database) -> Result<Connections, StoreError> {
    Ok(Connections {
        revision: settings_revision(db)?,
        gitlab: summary(db, Provider::GitLab)?,
        jira: summary(db, Provider::Jira)?,
        scope: scope::read(db)?,
        chat_setup: projection::summary(db)?,
    })
}

fn save(db: &Database, connection: SavedConnection) -> Result<(), StoreError> {
    let account = connection.account;
    let token = connection.token.ok_or(AppError::InvalidRequest)?;
    let provider = label(account.connection.provider());
    let (email, token_type) = match &account.connection {
        Connection::GitLab { .. } => (None, None),
        Connection::Jira {
            email, token_type, ..
        } => (
            Some(email.as_str()),
            Some(match token_type {
                JiraTokenType::Scoped => "scoped",
                JiraTokenType::Unscoped => "unscoped",
            }),
        ),
    };
    db.execute("INSERT INTO settings(provider,origin,email,token_type,cloud_id,identity_id,identity_label)
        VALUES(?1,?2,?3,?4,?5,?6,?7)
        ON CONFLICT(provider) DO UPDATE SET origin=excluded.origin,email=excluded.email,token_type=excluded.token_type,
            cloud_id=excluded.cloud_id,identity_id=excluded.identity_id,identity_label=excluded.identity_label",
        params![provider, account.connection.origin(), email, token_type, account.cloud_id, account.identity.id, account.identity.label])?;
    db.execute(
        "INSERT INTO credentials(provider,token) VALUES(?1,?2)
        ON CONFLICT(provider) DO UPDATE SET token=excluded.token",
        params![provider, token.expose()],
    )?;
    Ok(())
}

fn recheck(db: &Database, expected: Revision) -> Result<(), StoreError> {
    if settings_revision(db)? != expected {
        return Err(AppError::StaleRevision.into());
    }
    Ok(())
}

fn pinned(db: &Database) -> Result<bool, StoreError> {
    Ok(
        db.query_row("SELECT EXISTS(SELECT 1 FROM retained_reviews)", [], |row| {
            row.get(0)
        })?,
    )
}

fn label(provider: Provider) -> &'static str {
    match provider {
        Provider::GitLab => "gitlab",
        Provider::Jira => "jira",
    }
}

fn advance(tx: &mut WriteTx<'_>, expected: Revision) -> Result<Snapshot<Connections>, StoreError> {
    let next = expected.checked_add(1).ok_or(AppError::Internal)?;
    tx.sql.execute(
        "UPDATE service_state SET settings_revision=?1 WHERE singleton=1",
        [next.as_sqlite_integer()],
    )?;
    advance_queue(&tx.sql)?;
    tx.projection_changed = true;
    tx.emit(&[ResourceKey::Settings, ResourceKey::Queue])?;
    snapshot(&tx.sql, view(&tx.sql)?)
}
