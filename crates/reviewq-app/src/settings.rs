//! Bounded credential selection and provider validation for workspace settings.

use std::fmt;
use std::sync::Arc;

use reviewq_domain::identity::Revision;
use reviewq_domain::settings::{Connection, Provider, VerifiedAccount};

use crate::access::{Actor, AppError, AppFuture};
use crate::events::Snapshot;
use crate::sources::ReviewSourceReader;

#[derive(Clone)]
pub struct SecretToken(Box<str>);

impl SecretToken {
    pub fn new(value: String) -> Result<Self, AppError> {
        if value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control) {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self(value.into_boxed_str()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str("SecretToken(<private>)")
    }
}

pub enum TokenChange {
    Keep,
    Replace(SecretToken),
}

pub struct SavedConnection {
    pub account: VerifiedAccount,
    pub token: Option<SecretToken>,
}

pub struct DraftConnection {
    pub connection: Connection,
    pub token: TokenChange,
}

pub struct ConnectionAuthority {
    pub connection: Connection,
    pub token: SecretToken,
    pub pinned_account: Option<VerifiedAccount>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogKind {
    Projects(ProjectView),
    Boards,
    Board(u64),
    Statuses,
    Filters,
    Sprints(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectView {
    Member,
    Starred,
    All,
}

pub struct CatalogQuery {
    pub kind: CatalogKind,
    pub text: Option<String>,
    pub states: Vec<String>,
    pub offset: u32,
}

pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub next: Option<u32>,
}

pub struct CatalogItem {
    pub id: String,
    pub label: String,
    pub path: Option<String>,
    pub board_id: Option<u64>,
    pub state: Option<String>,
    pub board_type: Option<String>,
    pub status_ids: Vec<String>,
    pub estimate_field: Option<String>,
}

pub trait ConnectionSources: Send + Sync {
    /// Performs syntactic normalization before the application selects a stored token.
    fn normalize(&self, draft: Connection) -> Result<Connection, AppError>;

    fn verify(&self, authority: ConnectionAuthority) -> AppFuture<'_, VerifiedAccount>;

    fn catalog(
        &self,
        authority: ConnectionAuthority,
        query: CatalogQuery,
    ) -> AppFuture<'_, CatalogPage>;

    /// Creates a runtime reader for a previously verified active GitLab connection.
    fn review_source(
        &self,
        saved: &SavedConnection,
    ) -> Result<Arc<dyn ReviewSourceReader>, AppError>;
}

pub struct SavedCredentials {
    pub revision: Revision,
    pub pinned: bool,
    pub gitlab: Option<SavedConnection>,
    pub jira: Option<SavedConnection>,
}

impl SavedCredentials {
    pub fn connection(&self, provider: Provider) -> Option<&SavedConnection> {
        match provider {
            Provider::GitLab => self.gitlab.as_ref(),
            Provider::Jira => self.jira.as_ref(),
        }
    }
}

pub trait SettingsStore: Send + Sync {
    /// Private credentials are used only by the connection/projection use cases.
    fn read_credentials(&self, actor: Actor) -> AppFuture<'_, Snapshot<SavedCredentials>>;

    fn settings(&self, actor: Actor) -> AppFuture<'_, Snapshot<Connections>>;
    fn preference_revision(&self) -> AppFuture<'_, Revision>;

    fn activate_connections(
        &self,
        actor: Actor,
        expected: Revision,
        gitlab: SavedConnection,
        jira: SavedConnection,
    ) -> AppFuture<'_, Snapshot<Connections>>;

    fn disconnect(
        &self,
        actor: Actor,
        expected: Revision,
        provider: Provider,
    ) -> AppFuture<'_, Snapshot<Connections>>;
}

pub struct Connections {
    pub revision: Revision,
    pub gitlab: Option<ConnectionSummary>,
    pub jira: Option<ConnectionSummary>,
}

pub struct ConnectionSummary {
    pub account: VerifiedAccount,
    pub configured: bool,
}

pub fn source_authority(
    sources: &dyn ConnectionSources,
    saved: &SavedCredentials,
    expected: Revision,
    draft: DraftConnection,
) -> Result<ConnectionAuthority, AppError> {
    if saved.revision != expected {
        return Err(AppError::StaleRevision);
    }
    let connection = sources.normalize(draft.connection)?;
    let prior = saved.connection(connection.provider());
    if saved.pinned
        && prior.is_some_and(|prior| prior.account.connection.origin() != connection.origin())
    {
        return Err(AppError::Conflict);
    }
    let pin = saved.pinned || matches!(&draft.token, TokenChange::Keep);
    let token = match draft.token {
        TokenChange::Replace(value) => value,
        TokenChange::Keep => prior
            .filter(|prior| prior.account.connection == connection)
            .and_then(|prior| prior.token.clone())
            .ok_or(AppError::PreconditionFailed)?,
    };
    Ok(ConnectionAuthority {
        connection,
        token,
        pinned_account: prior.filter(|_| pin).map(|prior| prior.account.clone()),
    })
}

pub async fn verify_connection(
    sources: &dyn ConnectionSources,
    saved: &SavedCredentials,
    expected: Revision,
    draft: DraftConnection,
) -> Result<SavedConnection, AppError> {
    let authority = source_authority(sources, saved, expected, draft)?;
    let token = authority.token.clone();
    let verified = sources.verify(authority).await?;
    if saved.pinned
        && saved
            .connection(verified.connection.provider())
            .is_some_and(|prior| !prior.account.same_authority(&verified))
    {
        return Err(AppError::Conflict);
    }
    Ok(SavedConnection {
        account: verified,
        token: Some(token),
    })
}
