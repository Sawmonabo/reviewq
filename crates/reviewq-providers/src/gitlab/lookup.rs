use std::borrow::Cow;

use percent_encoding::percent_decode_str;
use reviewq_app::access::AppError;
use reviewq_app::queue::{MergeRequestState, ReviewSource};
use reviewq_app::sources::{ReviewResolution, UnverifiedTrack};
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::{MergeRequestIid, ProjectId};
use url::Url;

use super::{GitLab, MAX_PROVIDER_PAGES, METADATA_LIMIT, MergeRequest, Project};

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

impl GitLab {
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
        let (project, id, row) = self.direct_row(projects, &path, iid).await?;
        if row.title.len() > 4096 || row.source_branch.len() > 4096 {
            return Err(AppError::PayloadTooLarge);
        }
        let candidate_tickets = super::tickets::candidate_keys(&row.title, &row.source_branch)?;
        Ok(UnverifiedTrack {
            review: self.review(id, &project.path_with_namespace, row)?,
            candidate_tickets,
        })
    }

    pub(super) async fn resolve_target(
        &self,
        projects: Vec<ConfiguredProject>,
        target: String,
    ) -> Result<ReviewResolution, AppError> {
        if target.len() > 2_048 || target.is_empty() || target.trim() != target {
            return Err(AppError::InvalidRequest);
        }
        let projects: Vec<_> = projects
            .into_iter()
            .filter(|project| project.origin == self.identity)
            .map(|project| project.project_id)
            .collect();
        match Target::parse(&target)? {
            Target::Ticket(key) => {
                let mut found = Matches::default();
                for project in projects {
                    let metadata = self.project(project).await?;
                    self.match_ticket(project, &metadata.path_with_namespace, &key, &mut found)
                        .await?;
                }
                found.resolve(self)
            }
            Target::Web(url) => {
                let (path, iid) = self.web_address(&url)?;
                self.direct(projects, &path, iid)
                    .await
                    .map(ReviewResolution::Found)
            }
            Target::Project { path, iid } => self
                .direct(projects, path, iid)
                .await
                .map(ReviewResolution::Found),
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

    async fn direct(
        &self,
        projects: Vec<ProjectId>,
        path: &str,
        iid: MergeRequestIid,
    ) -> Result<ReviewSource, AppError> {
        let (project, id, row) = self.direct_row(projects, path, iid).await?;
        self.review(id, &project.path_with_namespace, row)
    }

    async fn direct_row(
        &self,
        projects: Vec<ProjectId>,
        path: &str,
        iid: MergeRequestIid,
    ) -> Result<(Project, ProjectId, MergeRequest), AppError> {
        if projects.is_empty() || !valid_path(path) {
            return Err(AppError::NotFound);
        }
        let mut url = self.url("api/v4/projects/")?;
        url.path_segments_mut()
            .map_err(|_| AppError::Internal)?
            .pop_if_empty()
            .push(path);
        let project = self.project_metadata(url).await?;
        let id = ProjectId::try_from(project.id).map_err(|_| AppError::Unavailable)?;
        if project.path_with_namespace != path || !projects.contains(&id) {
            return Err(AppError::NotFound);
        }
        let row = self.merge_request(id, iid).await?;
        Ok((project, id, row))
    }

    pub(super) async fn merge_request(
        &self,
        project: ProjectId,
        iid: MergeRequestIid,
    ) -> Result<MergeRequest, AppError> {
        let url = self.url(&format!("api/v4/projects/{project}/merge_requests/{iid}"))?;
        let (_, bytes) = self.get(url, METADATA_LIMIT).await?;
        let row: MergeRequest =
            serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if row.project_id != project.get() || row.iid != iid.get() {
            return Err(AppError::Conflict);
        }
        Ok(row)
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

    fn review_url(&self, row: &ReviewSource) -> Result<String, AppError> {
        let mut url = self.origin.clone();
        url.path_segments_mut()
            .map_err(|_| AppError::Internal)?
            .extend(row.project_path.split('/'))
            .extend(["-", "merge_requests", &row.identity.iid().to_string()]);
        Ok(url.into())
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

    fn resolve(self, gitlab: &GitLab) -> Result<ReviewResolution, AppError> {
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
            .map(|row| gitlab.review_url(row))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ReviewResolution::Ambiguous(urls))
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

fn valid_path(path: &str) -> bool {
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
