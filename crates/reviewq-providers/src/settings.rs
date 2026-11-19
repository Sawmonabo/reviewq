//! Connection verification and catalog reads share the service's source network.

use std::path::PathBuf;
use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::settings::{
    CatalogKind, CatalogPage, CatalogQuery, ConnectionAuthority, ConnectionSources, ProjectView,
    SecretToken,
};
use reviewq_domain::settings::{Connection, VerifiedAccount};
use url::{Host, Url};

mod scope;

use crate::jira::Jira;
use crate::{GitLab, SourceNetwork};

pub struct SourceAccounts {
    network: Arc<SourceNetwork>,
    scratch: PathBuf,
    fixture: bool,
}

impl SourceAccounts {
    pub fn new(network: Arc<SourceNetwork>, scratch: PathBuf, fixture: bool) -> Self {
        Self {
            network,
            scratch,
            fixture,
        }
    }

    pub fn gitlab(&self, origin: &str, token: &SecretToken) -> Result<GitLab, AppError> {
        GitLab::authenticated(
            origin,
            Some(token.expose()),
            self.scratch.clone(),
            self.network.clone(),
            self.fixture,
        )
    }
}

impl ConnectionSources for SourceAccounts {
    fn verify_draft_checkout<'a>(
        &'a self,
        gitlab: &'a reviewq_app::settings::SavedConnection,
        project: reviewq_domain::identity::ProjectId,
        checkout: reviewq_domain::session::AbsolutePath,
    ) -> AppFuture<'a, reviewq_domain::settings::CheckoutBinding> {
        Box::pin(async move {
            let Connection::GitLab { origin } = &gitlab.account.connection else {
                return Err(AppError::InvalidRequest);
            };
            let source =
                self.gitlab(origin, gitlab.token.as_ref().ok_or(AppError::Unauthorized)?)?;
            scope::checked_checkout(&source, project, &checkout).await
        })
    }
    fn verify_scope<'a>(
        &'a self,
        gitlab: &'a reviewq_app::settings::SavedConnection,
        jira: &'a reviewq_app::settings::SavedConnection,
        draft: reviewq_domain::settings::WorkspaceDraft,
    ) -> AppFuture<'a, reviewq_domain::settings::WorkspaceScope> {
        Box::pin(self.validate_workspace(gitlab, jira, draft))
    }
    fn review_source(
        &self,
        saved: &reviewq_app::settings::SavedConnection,
        lease: reviewq_app::settings::SourceLease,
    ) -> Result<Arc<dyn reviewq_app::sources::ReviewSourceReader>, AppError> {
        let Connection::GitLab { origin } = &saved.account.connection else {
            return Err(AppError::InvalidRequest);
        };
        let token = saved.token.as_ref().ok_or(AppError::Unauthorized)?;
        let account = saved
            .account
            .identity
            .id
            .parse::<u64>()
            .ok()
            .filter(|id| (1..=i64::MAX as u64).contains(id))
            .ok_or(AppError::Conflict)?;
        Ok(Arc::new(
            self.gitlab(origin, token)?
                .with_account(account)
                .with_lease(lease),
        ))
    }

    fn ticket_source(
        &self,
        saved: &reviewq_app::settings::SavedConnection,
        lease: reviewq_app::settings::SourceLease,
    ) -> Result<Arc<dyn reviewq_app::sources::TicketSourceReader>, AppError> {
        let token = saved.token.as_ref().ok_or(AppError::Unauthorized)?;
        let cloud = saved
            .account
            .cloud_id
            .as_deref()
            .ok_or(AppError::Conflict)?;
        Ok(Arc::new(Jira::for_bound(
            self.network.clone(),
            &saved.account.connection,
            token,
            cloud,
            self.fixture,
            Some(lease),
        )?))
    }

    fn normalize(&self, draft: Connection) -> Result<Connection, AppError> {
        let origin = origin(draft.origin(), self.fixture)?
            .origin()
            .ascii_serialization();
        Ok(match draft {
            Connection::GitLab { .. } => Connection::GitLab { origin },
            Connection::Jira {
                email, token_type, ..
            } => {
                let (local, domain) = email.split_once('@').ok_or(AppError::InvalidRequest)?;
                if local.is_empty()
                    || domain.is_empty()
                    || email.len() > 320
                    || email.contains(':')
                    || email
                        .chars()
                        .any(|ch| ch.is_control() || ch.is_whitespace())
                {
                    return Err(AppError::InvalidRequest);
                }
                Connection::Jira {
                    site: origin,
                    email,
                    token_type,
                }
            }
        })
    }

    fn verify(&self, authority: ConnectionAuthority) -> AppFuture<'_, VerifiedAccount> {
        Box::pin(async move {
            let ConnectionAuthority {
                connection,
                token,
                pinned_account,
            } = authority;
            let (identity, cloud_id) = match &connection {
                Connection::GitLab { origin } => {
                    let gitlab = self.gitlab(origin, &token)?;
                    let identity = gitlab.current_user().await?;
                    check_account(pinned_account.as_ref(), &identity.id)?;
                    gitlab
                        .project_catalog(CatalogQuery {
                            kind: CatalogKind::Projects(ProjectView::Member),
                            text: None,
                            states: Vec::new(),
                            offset: 0,
                        })
                        .await?;
                    (identity, None)
                }
                Connection::Jira { .. } => {
                    let cloud = pinned_account
                        .as_ref()
                        .and_then(|account| account.cloud_id.as_deref());
                    let jira = Jira::connect(
                        self.network.clone(),
                        &connection,
                        &token,
                        cloud,
                        self.fixture,
                    )
                    .await?;
                    let identity = jira.identity().await?;
                    check_account(pinned_account.as_ref(), &identity.id)?;
                    (identity, Some(jira.cloud_id().to_owned()))
                }
            };
            Ok(VerifiedAccount {
                connection,
                identity,
                cloud_id,
            })
        })
    }

    fn catalog(
        &self,
        authority: ConnectionAuthority,
        query: CatalogQuery,
    ) -> AppFuture<'_, CatalogPage> {
        Box::pin(async move {
            let ConnectionAuthority {
                connection,
                token,
                pinned_account,
            } = authority;
            if query.text.as_ref().is_some_and(|text| {
                text.chars().take(201).count() > 200 || text.chars().any(char::is_control)
            }) {
                return Err(AppError::InvalidRequest);
            }
            match &connection {
                Connection::GitLab { origin } => {
                    let gitlab = self.gitlab(origin, &token)?;
                    if pinned_account.is_some() {
                        check_account(pinned_account.as_ref(), &gitlab.current_user().await?.id)?;
                    }
                    gitlab.project_catalog(query).await
                }
                Connection::Jira { .. } => {
                    let cloud = pinned_account
                        .as_ref()
                        .and_then(|account| account.cloud_id.as_deref());
                    let jira = Jira::connect(
                        self.network.clone(),
                        &connection,
                        &token,
                        cloud,
                        self.fixture,
                    )
                    .await?;
                    if pinned_account.is_some() {
                        check_account(pinned_account.as_ref(), &jira.identity().await?.id)?;
                    }
                    jira.catalog(query).await
                }
            }
        })
    }
}

fn check_account(account: Option<&VerifiedAccount>, observed: &str) -> Result<(), AppError> {
    if account.is_some_and(|account| account.identity.id != observed) {
        return Err(AppError::Conflict);
    }
    Ok(())
}

pub(crate) fn origin(value: &str, fixture: bool) -> Result<Url, AppError> {
    if value.len() > 512 || value.chars().any(char::is_control) {
        return Err(AppError::InvalidRequest);
    }
    let url = Url::parse(value).map_err(|_| AppError::InvalidRequest)?;
    let loopback = matches!(url.host(), Some(Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(Host::Ipv6(ip)) if ip.is_loopback());
    let scheme = if fixture {
        loopback && matches!(url.scheme(), "http" | "https")
    } else {
        url.scheme() == "https"
    };
    if !scheme
        || url.host().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AppError::InvalidRequest);
    }
    Ok(url)
}

pub(crate) fn safe_label(value: &str, max: usize) -> Result<String, AppError> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(AppError::Unavailable);
    }
    Ok(value.to_owned())
}
