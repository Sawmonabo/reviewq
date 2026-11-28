use std::borrow::Cow;

use percent_encoding::percent_decode_str;
use reviewq_app::access::AppError;
use reviewq_app::queue::{MergeRequestState, ReviewSource};
use reviewq_app::sources::{
    AuthenticatedReviewHead, MergeRequestAddress, ReviewResolution, UnverifiedTrack,
};
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::{MergeRequestIid, MrIdentity, ProjectId};
use serde::Deserialize;
use url::Url;

use super::{
    GitLab, MAX_PROVIDER_PAGES, METADATA_LIMIT, MergeRequest, Project, merge_request_state,
};

const MAX_CANDIDATES: usize = 20;

enum Target<'a> {
    Web(Url),
    Project { path: &'a str, iid: MergeRequestIid },
    Ticket(String),
}

#[derive(Default)]
struct Matches {
    open: Vec<ReviewSource>,
    historical: Vec<ReviewSource>,
}

#[derive(Deserialize)]
struct MergeRequestHead {
    project_id: u64,
    iid: u64,
    sha: String,
    state: String,
}

impl GitLab {
    pub(super) async fn read_authenticated_review_head(
        &self,
        identity: MrIdentity,
    ) -> Result<AuthenticatedReviewHead, AppError> {
        if identity.origin() != &self.identity {
            return Err(AppError::NotFound);
        }
        let actor = self.observe_authenticated_actor().await?;
        if self.account.is_some_and(|expected| expected != actor) {
            return Err(AppError::Conflict);
        }
        let payload = self
            .merge_request_payload(&identity.project_id().to_string(), identity.iid())
            .await?;
        let row: MergeRequestHead =
            serde_json::from_slice(&payload).map_err(|_| AppError::Unavailable)?;
        if row.project_id != identity.project_id().get() || row.iid != identity.iid().get() {
            return Err(AppError::Conflict);
        }
        Ok(AuthenticatedReviewHead {
            identity,
            head_sha: row.sha.parse().map_err(|_| AppError::Unavailable)?,
            state: merge_request_state(&row.state)?,
            authenticated_actor: actor,
        })
    }

    pub(super) async fn track_url(
        &self,
        projects: Vec<ConfiguredProject>,
        url: String,
    ) -> Result<UnverifiedTrack, AppError> {
        if url.len() > 2_048 || url.trim() != url {
            return Err(AppError::InvalidRequest);
        }
        let Target::Web(url) = Target::parse(&url)? else {
            return Err(AppError::InvalidRequest);
        };
        let (path, iid) = self.web_address(&url)?;
        let projects = projects
            .into_iter()
            .filter(|item| item.origin == self.identity)
            .map(|item| item.project_id)
            .collect();
        let (id, row) = self.direct_row(projects, &path, iid).await?;
        if row.title.len() > 4096 || row.source_branch.len() > 4096 {
            return Err(AppError::PayloadTooLarge);
        }
        let candidate_tickets = super::tickets::candidate_keys(&row.title, &row.source_branch)?;
        Ok(UnverifiedTrack {
            review: self.review(id, &path, row)?,
            candidate_tickets,
        })
    }

    pub(super) fn target_address(
        &self,
        target: &str,
    ) -> Result<Option<MergeRequestAddress>, AppError> {
        Ok(match self.named(target)? {
            Named::MergeRequest(address) => Some(address),
            Named::Ticket(_) => None,
        })
    }

    /// Checks and reads a `resolve` target without a provider call.
    fn named(&self, target: &str) -> Result<Named, AppError> {
        if target.len() > 2_048 || target.is_empty() || target.trim() != target {
            return Err(AppError::InvalidRequest);
        }
        let (path, iid) = match Target::parse(target)? {
            Target::Ticket(key) => return Ok(Named::Ticket(key)),
            Target::Web(url) => {
                let (path, iid) = self.web_address(&url)?;
                (path.into_owned(), iid)
            }
            Target::Project { path, iid } => (path.to_owned(), iid),
        };
        Ok(Named::MergeRequest(MergeRequestAddress {
            origin: self.identity.clone(),
            project_path: path,
            iid,
        }))
    }

    pub(super) async fn resolve_target(
        &self,
        projects: Vec<ConfiguredProject>,
        target: String,
    ) -> Result<ReviewResolution, AppError> {
        let named = self.named(&target)?;
        let projects: Vec<_> = projects
            .into_iter()
            .filter(|project| project.origin == self.identity)
            .map(|project| project.project_id)
            .collect();
        match named {
            Named::Ticket(key) => {
                let mut found = Matches::default();
                for project in projects {
                    let metadata = self.project(project).await?;
                    self.match_ticket(project, &metadata.path_with_namespace, &key, &mut found)
                        .await?;
                }
                found.resolve()
            }
            Named::MergeRequest(address) => {
                let path = address.project_path;
                let (id, row) = self.direct_row(projects, &path, address.iid).await?;
                self.review(id, &path, row).map(ReviewResolution::Found)
            }
        }
    }

    async fn match_ticket(
        &self,
        project: ProjectId,
        path: &str,
        key: &str,
        found: &mut Matches,
    ) -> Result<(), AppError> {
        for page in 1..=MAX_PROVIDER_PAGES {
            let (rows, more) = self.merge_requests(project, page, "all").await?;
            for row in rows {
                if contains_key(&row.title, key) || contains_key(&row.source_branch, key) {
                    found.add(self.review(project, path, row)?);
                }
            }
            if !more {
                return Ok(());
            }
        }
        Err(AppError::PayloadTooLarge)
    }

    /// Reads the merge request at exactly `path` in one call. `NotFound` unless `path` is the
    /// project's current full path and the project is one of `projects`.
    async fn direct_row(
        &self,
        projects: Vec<ProjectId>,
        path: &str,
        iid: MergeRequestIid,
    ) -> Result<(ProjectId, MergeRequest), AppError> {
        if projects.is_empty() || !valid_path(path) {
            return Err(AppError::NotFound);
        }
        let row = match self.merge_request_at(path, iid).await {
            Ok(row) => row,
            Err(AppError::NotFound) => return Err(AppError::NotFound),
            Err(error) => {
                // A project that is unreadable, renamed or not configured stays not found, as
                // when its project was read first; only then does the failure stand.
                let project = self.project_by_path(path).await?;
                let id = ProjectId::try_from(project.id).map_err(|_| AppError::Unavailable)?;
                if project.path_with_namespace != path || !projects.contains(&id) {
                    return Err(AppError::NotFound);
                }
                return Err(error);
            }
        };
        let id = ProjectId::try_from(row.project_id).map_err(|_| AppError::Unavailable)?;
        if !projects.contains(&id) || project_path(&row)? != path {
            return Err(AppError::NotFound);
        }
        Ok((id, row))
    }

    pub(super) async fn project_by_path(&self, path: &str) -> Result<Project, AppError> {
        let mut url = self.url("api/v4/projects/")?;
        url.path_segments_mut()
            .map_err(|_| AppError::Internal)?
            .pop_if_empty()
            .push(path);
        self.project_metadata(url).await
    }

    pub(super) async fn merge_request(
        &self,
        project: ProjectId,
        iid: MergeRequestIid,
    ) -> Result<MergeRequest, AppError> {
        let row = self.merge_request_at(&project.to_string(), iid).await?;
        if row.project_id != project.get() {
            return Err(AppError::Conflict);
        }
        Ok(row)
    }

    /// Reads a merge request by its project's numeric ID or full path.
    async fn merge_request_at(
        &self,
        project: &str,
        iid: MergeRequestIid,
    ) -> Result<MergeRequest, AppError> {
        let bytes = self.merge_request_payload(project, iid).await?;
        let row: MergeRequest =
            serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if row.iid != iid.get() {
            return Err(AppError::Conflict);
        }
        Ok(row)
    }

    async fn merge_request_payload(
        &self,
        project: &str,
        iid: MergeRequestIid,
    ) -> Result<Vec<u8>, AppError> {
        let mut url = self.url("api/v4/projects/")?;
        url.path_segments_mut()
            .map_err(|_| AppError::Internal)?
            .pop_if_empty()
            .push(project)
            .extend(["merge_requests", &iid.to_string()]);
        self.get(url, METADATA_LIMIT).await.map(|(_, bytes)| bytes)
    }

    fn web_address<'a>(&self, url: &'a Url) -> Result<(Cow<'a, str>, MergeRequestIid), AppError> {
        if url.origin() != self.origin.origin() {
            return Err(AppError::NotFound);
        }
        let (path, iid) = url
            .path()
            .strip_prefix('/')
            .and_then(|raw| raw.rsplit_once("/-/merge_requests/"))
            .ok_or(AppError::InvalidRequest)?;
        let path = percent_decode_str(path)
            .decode_utf8()
            .map_err(|_| AppError::InvalidRequest)?;
        Ok((path, iid.parse().map_err(|_| AppError::InvalidRequest)?))
    }
}

impl Matches {
    fn add(&mut self, candidate: ReviewSource) {
        let group = if candidate.state == MergeRequestState::Open {
            &mut self.open
        } else {
            &mut self.historical
        };
        if group.len() < MAX_CANDIDATES
            && !group.iter().any(|row| row.identity == candidate.identity)
        {
            group.push(candidate);
        }
    }

    fn resolve(self) -> Result<ReviewResolution, AppError> {
        let mut candidates = if self.open.is_empty() {
            self.historical
        } else {
            self.open
        };
        if candidates.len() == 1 {
            return candidates
                .pop()
                .map(ReviewResolution::Found)
                .ok_or(AppError::NotFound);
        }
        if candidates.is_empty() {
            return Err(AppError::NotFound);
        }
        let urls = candidates
            .iter()
            .map(ReviewSource::web_url)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ReviewResolution::Ambiguous(urls))
    }
}

enum Named {
    MergeRequest(MergeRequestAddress),
    Ticket(String),
}

/// The current full path of the row's project, from its `references.full`
/// (`group/project!iid`).
pub(super) fn project_path(row: &MergeRequest) -> Result<String, AppError> {
    let full = &row.references.as_ref().ok_or(AppError::Unavailable)?.full;
    match Target::parse(full) {
        Ok(Target::Project { path, iid }) if iid.get() == row.iid => Ok(path.to_owned()),
        _ => Err(AppError::Unavailable),
    }
}

impl<'a> Target<'a> {
    fn parse(target: &'a str) -> Result<Self, AppError> {
        if let Ok(url) = Url::parse(target) {
            if !url.username().is_empty()
                || url.password().is_some()
                || !matches!(url.scheme(), "http" | "https")
            {
                return Err(AppError::InvalidRequest);
            }
            return Ok(Self::Web(url));
        }
        if let Some((project, iid)) = target.rsplit_once('!') {
            if !valid_path(project) {
                return Err(AppError::InvalidRequest);
            }
            return Ok(Self::Project {
                path: project,
                iid: iid.parse().map_err(|_| AppError::InvalidRequest)?,
            });
        }
        let (prefix, number) = target.split_once('-').ok_or(AppError::InvalidRequest)?;
        if prefix.is_empty()
            || prefix.len() > 40
            || number.is_empty()
            || number.len() > 19
            || !prefix.bytes().all(|byte| byte.is_ascii_alphanumeric())
            || !number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(AppError::InvalidRequest);
        }
        Ok(Self::Ticket(target.to_ascii_uppercase()))
    }
}

pub(super) fn valid_path(path: &str) -> bool {
    path.len() <= 512
        && path.contains('/')
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.chars().any(char::is_control)
        })
}

fn contains_key(value: &str, key: &str) -> bool {
    value
        .to_ascii_uppercase()
        .match_indices(key)
        .any(|(offset, _)| key_boundaries(value, offset, offset + key.len()))
}

pub(super) fn key_boundaries(value: &str, start: usize, end: usize) -> bool {
    !value[..start]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
        && !value[end..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
}
