use std::sync::Arc;

use reqwest::header;
use reviewq_app::access::{AppError, AppFuture, Service};
use reviewq_app::queue::{MergeRequestState, ReviewAuthor, ReviewSource};
use reviewq_app::settings::SourceLease;
use reviewq_app::sources::{
    AuthenticatedReviewHead, MatchedReview, MergeRequestAddress, ReviewResolution,
    ReviewSourceReader, UnverifiedTrack, WorkflowSnapshot,
};
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::{GitLabOrigin, GitSha, MrIdentity, ProjectId, TicketKey};
use serde::Deserialize;
use url::{Host, Url};

use crate::http::SourceNetwork;

mod changes;
mod checkout;
mod lookup;
mod reviewers;
mod settings;
mod tickets;

const METADATA_LIMIT: usize = 1024 * 1024;
const MERGE_REQUEST_PAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PROVIDER_PAGES: u32 = 40;

/// A GitLab client pinned to one configured, verified transport origin.
#[derive(Clone)]
pub struct GitLab {
    origin: Url,
    identity: GitLabOrigin,
    http: Arc<SourceNetwork>,
    headers: header::HeaderMap,
    patches: changes::Leases,
    discussions: Arc<changes::DiscussionCache>,
    lease: Option<SourceLease>,
    account: Option<u64>,
}

impl ReviewSourceReader for GitLab {
    fn verify_checkout(
        &self,
        project: ConfiguredProject,
        remote: String,
    ) -> AppFuture<'_, ConfiguredProject> {
        Box::pin(self.verify_checkout_remote(project, remote))
    }

    fn verify_launch_checkout(
        &self,
        project: ConfiguredProject,
        checkout: reviewq_domain::settings::CheckoutBinding,
    ) -> AppFuture<'_, ()> {
        Box::pin(async move {
            let (observed, remote) = crate::checkout::verify_checkout(&checkout.root).await?;
            if observed != checkout {
                return Err(AppError::Conflict);
            }
            self.verify_checkout_remote(project, remote).await?;
            Ok(())
        })
    }
    fn review(&self, identity: MrIdentity) -> AppFuture<'_, ReviewSource> {
        Box::pin(async move {
            if identity.origin() != &self.identity {
                return Err(AppError::NotFound);
            }
            let mr = self
                .merge_request(identity.project_id(), identity.iid())
                .await?;
            let path = lookup::project_path(&mr)?;
            self.review(identity.project_id(), &path, mr)
        })
    }

    fn workflow_snapshot(&self, identity: MrIdentity) -> AppFuture<'_, WorkflowSnapshot> {
        Box::pin(self.read_workflow_snapshot(identity))
    }

    fn authenticated_review_head(
        &self,
        identity: MrIdentity,
    ) -> AppFuture<'_, AuthenticatedReviewHead> {
        Box::pin(self.read_authenticated_review_head(identity))
    }

    fn discovery_evidence(
        &self,
        identity: MrIdentity,
    ) -> AppFuture<'_, reviewq_app::sources::DiscoveryEvidence> {
        Box::pin(self.read_discovery_evidence(identity))
    }

    fn trackable_review(
        &self,
        projects: Vec<ConfiguredProject>,
        canonical_url: String,
    ) -> AppFuture<'_, UnverifiedTrack> {
        Box::pin(self.track_url(projects, canonical_url))
    }

    fn reviews(&self, project: ConfiguredProject) -> AppFuture<'_, Vec<ReviewSource>> {
        Box::pin(async move {
            if project.origin != self.identity {
                return Err(AppError::NotFound);
            }
            self.list_reviews(project.project_id).await
        })
    }

    fn ticket_reviews(
        &self,
        project: ConfiguredProject,
        keys: Vec<TicketKey>,
    ) -> AppFuture<'_, Vec<MatchedReview>> {
        Box::pin(self.match_open_reviews(project, keys))
    }

    fn project_history_page(
        &self,
        project: ConfiguredProject,
        page: u32,
        since: Option<i64>,
    ) -> AppFuture<'_, reviewq_app::sources::ProjectHistoryPage> {
        Box::pin(self.history_page(project, page, since))
    }

    fn verify_history_candidates(
        &self,
        project: ConfiguredProject,
        candidates: Vec<reviewq_app::sources::HistoryCandidate>,
    ) -> AppFuture<'_, Vec<MatchedReview>> {
        Box::pin(self.history_candidates(project, candidates))
    }

    fn resolve(
        &self,
        projects: Vec<ConfiguredProject>,
        target: String,
    ) -> AppFuture<'_, ReviewResolution> {
        Box::pin(self.resolve_target(projects, target))
    }

    fn address(&self, target: &str) -> Result<Option<MergeRequestAddress>, AppError> {
        self.target_address(target)
    }

    fn rate_pause(&self) -> std::time::Duration {
        self.http.paused_for(&self.origin)
    }
}

#[derive(Deserialize)]
struct Project {
    id: u64,
    path_with_namespace: String,
}

#[derive(Deserialize)]
struct MergeRequest {
    id: Option<u64>,
    project_id: u64,
    iid: u64,
    title: String,
    sha: Option<String>,
    state: String,
    updated_at: String,
    source_branch: String,
    author: Option<Author>,
    #[serde(default)]
    draft: bool,
    diff_refs: Option<changes::DiffRefs>,
    changes_count: Option<serde_json::Value>,
    references: Option<References>,
}

#[derive(Deserialize)]
struct References {
    full: String,
}

impl MergeRequest {
    fn head_sha(&self) -> Result<GitSha, AppError> {
        self.sha
            .as_deref()
            .ok_or(AppError::Unavailable)?
            .parse()
            .map_err(|_| AppError::Unavailable)
    }
}

#[derive(Deserialize)]
struct Author {
    id: u64,
    username: String,
}

impl GitLab {
    /// Allows HTTP only on numeric loopback without credentials.
    pub fn new(
        origin: &str,
        token: Option<&str>,
        profile: PathBuf,
        http: Arc<SourceNetwork>,
    ) -> Result<Self, AppError> {
        Self::authenticated(origin, token, profile, http, false)
    }

    pub(crate) fn authenticated(
        origin: &str,
        token: Option<&str>,
        profile: PathBuf,
        http: Arc<SourceNetwork>,
        fixture: bool,
    ) -> Result<Self, AppError> {
        let origin = Url::parse(origin).map_err(|_| AppError::InvalidRequest)?;
        let local = match origin.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            _ => false,
        };
        if origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || (origin.scheme() != "https"
                && !(origin.scheme() == "http" && local && (token.is_none() || fixture)))
        {
            return Err(AppError::InvalidRequest);
        }
        let identity = GitLabOrigin::from_canonical(&origin.origin().ascii_serialization())
            .map_err(|_| AppError::InvalidRequest)?;
        let mut headers = header::HeaderMap::new();
        if let Some(token) = token {
            let mut value =
                header::HeaderValue::from_str(token).map_err(|_| AppError::InvalidRequest)?;
            value.set_sensitive(true);
            headers.insert(header::HeaderName::from_static("private-token"), value);
        }
        Ok(Self {
            origin,
            identity,
            http,
            headers,
            patches: changes::Leases::new(profile),
            discussions: Arc::default(),
            lease: None,
            account: None,
        })
    }

    pub fn configured_project(&self, project_id: ProjectId) -> ConfiguredProject {
        ConfiguredProject {
            origin: self.identity.clone(),
            project_id,
        }
    }

    pub(crate) fn with_lease(mut self, lease: SourceLease) -> Self {
        self.lease = Some(lease);
        self
    }

    pub(crate) fn with_account(mut self, actor: u64) -> Self {
        self.account = Some(actor);
        self
    }

    /// Reads verified metadata for the configured project before store observation.
    pub async fn list_reviews(&self, project_id: ProjectId) -> Result<Vec<ReviewSource>, AppError> {
        let project = self.project(project_id).await?;
        let mut reviews = Vec::new();
        for page in 1..=MAX_PROVIDER_PAGES {
            let (rows, more) = self.merge_requests(project_id, page, "all").await?;
            for row in rows {
                reviews.push(self.review(project_id, &project.path_with_namespace, row)?);
            }
            if !more {
                return Ok(reviews);
            }
        }
        Err(AppError::PayloadTooLarge)
    }

    async fn merge_requests(
        &self,
        project: ProjectId,
        page: u32,
        state: &str,
    ) -> Result<(Vec<MergeRequest>, bool), AppError> {
        self.merge_request_page(project, page, state, false, None)
            .await
    }

    async fn merge_request_page(
        &self,
        project: ProjectId,
        page: u32,
        state: &str,
        history: bool,
        since: Option<i64>,
    ) -> Result<(Vec<MergeRequest>, bool), AppError> {
        let mut url = self.url(&format!("api/v4/projects/{project}/merge_requests"))?;
        url.query_pairs_mut()
            .append_pair("page", &page.to_string())
            .append_pair("per_page", "100")
            .append_pair("scope", "all")
            .append_pair("state", state);
        if history {
            url.query_pairs_mut()
                .append_pair("order_by", "updated_at")
                .append_pair("sort", "asc");
            if let Some(since) = since {
                let time = jiff::Timestamp::from_millisecond(since)
                    .map_err(|_| AppError::InvalidRequest)?;
                url.query_pairs_mut()
                    .append_pair("updated_after", &time.to_string());
            }
        }
        let response = self
            .http
            .get(
                Service::GitLab,
                url,
                self.headers.clone(),
                self.lease.as_ref(),
            )
            .await?;
        let more = next_page(response.headers(), page)?.is_some();
        let body = response
            .bounded_reader(
                &self.patches.profile,
                METADATA_LIMIT,
                MERGE_REQUEST_PAGE_BYTES,
            )
            .await?;
        let rows: Vec<MergeRequest> =
            tokio::task::spawn_blocking(move || serde_json::from_reader(body))
                .await
                .map_err(|_| AppError::Unavailable)?
                .map_err(|_| AppError::Unavailable)?;
        if let Some(lease) = &self.lease {
            lease.check().await?;
        }
        if rows.len() > 100 || rows.iter().any(|row| row.project_id != project.get()) {
            return Err(AppError::Unavailable);
        }
        if more && rows.is_empty() {
            return Err(AppError::Unavailable);
        }
        Ok((rows, more))
    }

    async fn project(&self, project_id: ProjectId) -> Result<Project, AppError> {
        let url = self.url(&format!("api/v4/projects/{project_id}"))?;
        let project = self.project_metadata(url).await?;
        if project.id != project_id.get() {
            return Err(AppError::Conflict);
        }
        Ok(project)
    }

    async fn project_metadata(&self, url: Url) -> Result<Project, AppError> {
        let (_, bytes) = self.get(url, METADATA_LIMIT).await?;
        let project: Project = serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if project.path_with_namespace.is_empty()
            || project.path_with_namespace.len() > 512
            || project.path_with_namespace.chars().any(char::is_control)
        {
            return Err(AppError::Unavailable);
        }
        Ok(project)
    }

    fn review(
        &self,
        project_id: ProjectId,
        project_path: &str,
        row: MergeRequest,
    ) -> Result<ReviewSource, AppError> {
        if row.project_id != project_id.get() {
            return Err(AppError::Conflict);
        }
        if row.title.len() > 4096 {
            return Err(AppError::PayloadTooLarge);
        }
        let head_sha = row.head_sha()?;
        let state = merge_request_state(&row.state)?;
        let updated_at = row
            .updated_at
            .parse::<jiff::Timestamp>()
            .map_err(|_| AppError::Unavailable)?;
        Ok(ReviewSource {
            identity: MrIdentity::new(
                self.identity.clone(),
                project_id,
                row.iid.try_into().map_err(|_| AppError::Unavailable)?,
            ),
            project_path: project_path.into(),
            title: row.title,
            author: row
                .author
                .map(|author| {
                    if !(1..=i64::MAX as u64).contains(&author.id) {
                        return Err(AppError::Unavailable);
                    }
                    Ok(ReviewAuthor {
                        id: author.id,
                        username: crate::settings::safe_label(&author.username, 256)?,
                    })
                })
                .transpose()?,
            draft: row.draft,
            head_sha,
            state,
            updated_at_ms: updated_at.as_millisecond(),
        })
    }

    fn url(&self, path: &str) -> Result<Url, AppError> {
        self.origin.join(path).map_err(|_| AppError::Internal)
    }

    async fn get(&self, url: Url, max: usize) -> Result<(header::HeaderMap, Vec<u8>), AppError> {
        if url.origin() != self.origin.origin() {
            return Err(AppError::Forbidden);
        }
        self.http
            .bounded_response(
                Service::GitLab,
                url,
                self.headers.clone(),
                max,
                self.lease.as_ref(),
            )
            .await
    }
}

fn next_page(headers: &header::HeaderMap, page: u32) -> Result<Option<u32>, AppError> {
    let Some(value) = headers.get("x-next-page").filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let next: u32 = value
        .to_str()
        .ok()
        .and_then(|raw| raw.parse().ok())
        .ok_or(AppError::Unavailable)?;
    if page.checked_add(1) != Some(next) {
        return Err(AppError::Unavailable);
    }
    Ok(Some(next))
}

fn merge_request_state(state: &str) -> Result<MergeRequestState, AppError> {
    match state {
        "opened" => Ok(MergeRequestState::Open),
        "closed" => Ok(MergeRequestState::Closed),
        "merged" => Ok(MergeRequestState::Merged),
        _ => Err(AppError::Unavailable),
    }
}
use std::path::PathBuf;
