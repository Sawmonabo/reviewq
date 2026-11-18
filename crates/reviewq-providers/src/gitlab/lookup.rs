use reviewq_app::access::AppError;
use reviewq_app::queue::ReviewSource;
use reviewq_app::workspace::ConfiguredProject;
use reviewq_domain::identity::{MergeRequestIid, ProjectId};
use url::Url;

use super::{GitLab, MAX_PROVIDER_PAGES, METADATA_LIMIT, MergeRequest};

enum Target<'a> {
    Web(Url),
    Project { path: &'a str, iid: MergeRequestIid },
    Ticket(String),
}

impl GitLab {
    pub(super) async fn resolve_target(
        &self,
        projects: Vec<ConfiguredProject>,
        target: String,
    ) -> Result<ReviewSource, AppError> {
        if target.len() > 2_048 || target.is_empty() || target.trim() != target {
            return Err(AppError::InvalidRequest);
        }
        let target = Target::parse(&target)?;
        let mut found = None;
        for project in projects
            .into_iter()
            .filter(|project| project.origin == self.identity)
        {
            let metadata = self.project(project.project_id).await?;
            match &target {
                Target::Ticket(key) => {
                    self.match_ticket(
                        project.project_id,
                        &metadata.path_with_namespace,
                        key,
                        &mut found,
                    )
                    .await?;
                }
                _ => {
                    if let Some(iid) = self.direct_match(&target, &metadata.path_with_namespace)? {
                        let row = self.merge_request(project.project_id, iid).await?;
                        add(
                            &mut found,
                            self.review(project.project_id, &metadata.path_with_namespace, row)?,
                        )?;
                    }
                }
            }
        }
        found.ok_or(AppError::NotFound)
    }

    async fn match_ticket(
        &self,
        project: ProjectId,
        path: &str,
        key: &str,
        found: &mut Option<ReviewSource>,
    ) -> Result<(), AppError> {
        for page in 1..=MAX_PROVIDER_PAGES {
            let (rows, more) = self.merge_requests(project, page).await?;
            for row in rows {
                if contains_key(&row.title, key) || contains_key(&row.source_branch, key) {
                    add(found, self.review(project, path, row)?)?;
                }
            }
            if !more {
                return Ok(());
            }
        }
        Err(AppError::PayloadTooLarge)
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

    fn direct_match(
        &self,
        target: &Target<'_>,
        path: &str,
    ) -> Result<Option<MergeRequestIid>, AppError> {
        match target {
            Target::Project {
                path: requested,
                iid,
            } => Ok((*requested == path).then_some(*iid)),
            Target::Web(url) => {
                if url.origin() != self.origin.origin() {
                    return Err(AppError::NotFound);
                }
                let Some(iid) = url
                    .path_segments()
                    .and_then(Iterator::last)
                    .and_then(|value| value.parse::<MergeRequestIid>().ok())
                else {
                    return Err(AppError::InvalidRequest);
                };
                let mut expected = self.origin.clone();
                expected
                    .path_segments_mut()
                    .map_err(|_| AppError::Internal)?
                    .extend(path.split('/'))
                    .extend(["-", "merge_requests", &iid.to_string()]);
                Ok((url.path() == expected.path()).then_some(iid))
            }
            Target::Ticket(_) => Err(AppError::Internal),
        }
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
            if project.is_empty() {
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

fn add(found: &mut Option<ReviewSource>, candidate: ReviewSource) -> Result<(), AppError> {
    if found
        .as_ref()
        .is_some_and(|review| review.identity != candidate.identity)
    {
        return Err(AppError::Conflict);
    }
    *found = Some(candidate);
    Ok(())
}

fn contains_key(value: &str, key: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    upper.match_indices(key).any(|(offset, _)| {
        let end = offset + key.len();
        !value[..offset]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            && !value[end..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
    })
}
