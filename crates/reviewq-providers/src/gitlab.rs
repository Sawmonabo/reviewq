use reqwest::{Client, header};
use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::queue::{MergeRequestState, ReviewSource};
use reviewq_app::sources::{ReviewResolution, ReviewSourceReader};
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::{GitLabOrigin, MrIdentity, ProjectId};
use serde::Deserialize;
use url::{Host, Url};

use crate::http;

mod changes;
mod lookup;

const METADATA_LIMIT: usize = 1024 * 1024;
const MAX_PROVIDER_PAGES: u32 = 40;

/// A GitLab client pinned to one configured, verified transport origin.
pub struct GitLab {
    origin: Url,
    identity: GitLabOrigin,
    http: Client,
    headers: header::HeaderMap,
}

impl ReviewSourceReader for GitLab {
    fn reviews(&self, project: ConfiguredProject) -> AppFuture<'_, Vec<ReviewSource>> {
        Box::pin(async move {
            if project.origin != self.identity {
                return Err(AppError::NotFound);
            }
            self.list_reviews(project.project_id).await
        })
    }

    fn resolve(
        &self,
        projects: Vec<ConfiguredProject>,
        target: String,
    ) -> AppFuture<'_, ReviewResolution> {
        Box::pin(self.resolve_target(projects, target))
    }
}

#[derive(Deserialize)]
struct Project {
    id: u64,
    path_with_namespace: String,
}

#[derive(Deserialize)]
struct MergeRequest {
    project_id: u64,
    iid: u64,
    title: String,
    sha: String,
    state: String,
    updated_at: String,
    source_branch: String,
    diff_refs: Option<changes::DiffRefs>,
}

impl GitLab {
    /// Allows HTTP only on numeric loopback without credentials.
    pub fn new(origin: &str, token: Option<&str>) -> Result<Self, AppError> {
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
                && !(origin.scheme() == "http" && local && token.is_none()))
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
            http: http::client(local)?,
            headers,
        })
    }

    pub fn configured_project(&self, project_id: ProjectId) -> ConfiguredProject {
        ConfiguredProject {
            origin: self.identity.clone(),
            project_id,
        }
    }

    /// Reads verified metadata for the configured project before store observation.
    pub async fn list_reviews(&self, project_id: ProjectId) -> Result<Vec<ReviewSource>, AppError> {
        let project = self.project(project_id).await?;
        let mut reviews = Vec::new();
        for page in 1..=MAX_PROVIDER_PAGES {
            let (rows, more) = self.merge_requests(project_id, page).await?;
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
    ) -> Result<(Vec<MergeRequest>, bool), AppError> {
        let mut url = self.url(&format!("api/v4/projects/{project}/merge_requests"))?;
        url.query_pairs_mut()
            .append_pair("page", &page.to_string())
            .append_pair("per_page", "100");
        let (metadata, bytes) = self.get(url, METADATA_LIMIT).await?;
        let rows: Vec<MergeRequest> =
            serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if rows.len() > 100 || rows.iter().any(|row| row.project_id != project.get()) {
            return Err(AppError::Unavailable);
        }
        let more = match metadata.get("x-next-page") {
            None => false,
            Some(value) if value.is_empty() => false,
            Some(value) => {
                if value.to_str().ok().and_then(|value| value.parse().ok()) != Some(page + 1) {
                    return Err(AppError::Unavailable);
                }
                true
            }
        };
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
        let state = match row.state.as_str() {
            "opened" => MergeRequestState::Open,
            "closed" => MergeRequestState::Closed,
            "merged" => MergeRequestState::Merged,
            _ => return Err(AppError::Unavailable),
        };
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
            head_sha: row.sha.parse().map_err(|_| AppError::Unavailable)?,
            state,
            updated_at_ms: updated_at.as_millisecond(),
        })
    }

    fn url(&self, path: &str) -> Result<Url, AppError> {
        self.origin.join(path).map_err(|_| AppError::Internal)
    }

    async fn get(&self, url: Url, max: usize) -> Result<(header::HeaderMap, Vec<u8>), AppError> {
        http::bounded_response(&self.http, url, self.headers.clone(), max).await
    }
}
