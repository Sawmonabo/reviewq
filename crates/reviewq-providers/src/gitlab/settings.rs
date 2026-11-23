use std::borrow::Cow;
use std::collections::HashSet;

use percent_encoding::percent_decode_str;
use reviewq_app::access::AppError;
use reviewq_app::settings::{CatalogItem, CatalogKind, CatalogPage, CatalogQuery, ProjectView};
use reviewq_domain::identity::ProjectId;
use reviewq_domain::settings::Identity;
use serde::Deserialize;
use url::Url;

use super::{GitLab, METADATA_LIMIT, Project, next_page};

enum ProjectLookup<'a> {
    Id(Option<ProjectId>),
    Path {
        path: Cow<'a, str>,
        search_on_miss: bool,
    },
}

#[derive(Deserialize)]
struct User {
    id: u64,
    username: String,
}

impl GitLab {
    pub(crate) async fn verify_project(&self, id: ProjectId) -> Result<String, AppError> {
        Ok(self.project(id).await?.path_with_namespace)
    }
    pub(crate) async fn current_user(&self) -> Result<Identity, AppError> {
        let (_, body) = self.get(self.url("api/v4/user")?, METADATA_LIMIT).await?;
        let user: User = serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        if !(1..=i64::MAX as u64).contains(&user.id) {
            return Err(AppError::Unavailable);
        }
        let label = crate::settings::safe_label(&user.username, 512)?;
        Ok(Identity {
            id: user.id.to_string(),
            label,
        })
    }

    pub(crate) async fn project_catalog(
        &self,
        query: CatalogQuery,
    ) -> Result<CatalogPage, AppError> {
        let CatalogKind::Projects(view) = query.kind else {
            return Err(AppError::InvalidRequest);
        };
        if query.offset >= 2000 || !query.states.is_empty() {
            return Err(AppError::InvalidRequest);
        }
        let text = query
            .text
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty());
        let lookup = text
            .map(|text| self.project_lookup(text))
            .transpose()?
            .flatten();
        let search_namespaces = matches!(
            lookup,
            Some(ProjectLookup::Path {
                search_on_miss: true,
                ..
            })
        );
        if let Some(target) = lookup {
            if query.offset == 0 {
                let exact = self.exact_project(target, view).await?;
                if !search_namespaces || !exact.items.is_empty() {
                    return Ok(exact);
                }
            } else if !search_namespaces {
                return Err(AppError::InvalidRequest);
            }
        }
        let page = query.offset + 1;
        let mut url = self.url("api/v4/projects")?;
        apply_view(&mut url, view);
        {
            let mut params = url.query_pairs_mut();
            params
                .append_pair("page", &page.to_string())
                .append_pair("per_page", "100")
                .append_pair("simple", "true");
            if let Some(text) = text {
                params.append_pair("search", text);
            }
            if search_namespaces {
                params.append_pair("search_namespaces", "true");
            }
        }
        let (headers, body) = self.get(url, METADATA_LIMIT).await?;
        let projects: Vec<Project> =
            serde_json::from_slice(&body).map_err(|_| AppError::Unavailable)?;
        if projects.len() > 100 {
            return Err(AppError::PayloadTooLarge);
        }
        let mut ids = HashSet::with_capacity(projects.len());
        let items = projects
            .into_iter()
            .map(|project| {
                let id = ProjectId::try_from(project.id).map_err(|_| AppError::Unavailable)?;
                if !ids.insert(id) {
                    return Err(AppError::Unavailable);
                }
                project_item(id, project.path_with_namespace)
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        let next = match headers.get("x-next-page") {
            None => None,
            Some(value) if value.is_empty() => None,
            Some(value)
                if !items.is_empty()
                    && value
                        .to_str()
                        .ok()
                        .and_then(|value| value.parse::<u32>().ok())
                        == Some(page + 1)
                    && page < 2000 =>
            {
                Some(page)
            }
            _ => return Err(AppError::Unavailable),
        };
        Ok(CatalogPage { items, next })
    }

    fn project_lookup<'a>(&self, text: &'a str) -> Result<Option<ProjectLookup<'a>>, AppError> {
        if let Some(id) = numeric_lookup(text.strip_prefix('#').unwrap_or(text)) {
            return Ok(Some(id));
        }
        if text.starts_with("//") {
            return Err(AppError::InvalidRequest);
        }
        if text.contains("://") {
            let url = Url::parse(text).map_err(|_| AppError::InvalidRequest)?;
            if url.scheme() == "ssh" {
                let path = self
                    .ssh_project_path(&url)
                    .map_err(|_| AppError::InvalidRequest)?;
                return clone_lookup(path).map(Some);
            }
            if url.origin() != self.origin.origin()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(AppError::InvalidRequest);
            }
            let path = url.path().trim_matches('/');
            let path = path.split_once("/-/").map_or(path, |(path, _)| path);
            if let Some(id) = path.strip_prefix("projects/").and_then(numeric_lookup) {
                return Ok(Some(id));
            }
            return clone_lookup(path).map(Some);
        }
        if text.starts_with("git@") {
            let path = self
                .scp_project_path(text)
                .map_err(|_| AppError::InvalidRequest)?;
            return clone_lookup(path).map(Some);
        }
        let path = trim_git_suffix(text.trim_end_matches('/'));
        Ok(catalog_project_path(path).then_some(ProjectLookup::Path {
            path: Cow::Borrowed(path),
            search_on_miss: true,
        }))
    }

    async fn exact_project(
        &self,
        target: ProjectLookup<'_>,
        view: ProjectView,
    ) -> Result<CatalogPage, AppError> {
        let result = match &target {
            ProjectLookup::Id(Some(id)) => self.project(*id).await,
            ProjectLookup::Id(None) => Err(AppError::NotFound),
            ProjectLookup::Path { path, .. } => self.project_by_path(path).await,
        };
        let project = match result {
            Ok(project) => project,
            Err(AppError::NotFound) => {
                return Ok(CatalogPage {
                    items: Vec::new(),
                    next: None,
                });
            }
            Err(error) => return Err(error),
        };
        if let ProjectLookup::Path { path, .. } = target
            && !project.path_with_namespace.eq_ignore_ascii_case(&path)
        {
            return Err(AppError::Conflict);
        }
        let id = ProjectId::try_from(project.id).map_err(|_| AppError::Unavailable)?;
        let mut item = project_item(id, project.path_with_namespace)?;
        if view != ProjectView::All && !self.project_in_view(id, view).await? {
            item.outside_view = Some(true);
        }
        Ok(CatalogPage {
            items: vec![item],
            next: None,
        })
    }

    async fn project_in_view(&self, id: ProjectId, view: ProjectView) -> Result<bool, AppError> {
        let mut url = self.url("api/v4/projects")?;
        apply_view(&mut url, view);
        {
            let mut params = url.query_pairs_mut();
            params
                .append_pair("page", "1")
                .append_pair("per_page", "1")
                .append_pair("simple", "true")
                .append_pair("id_after", &(id.get() - 1).to_string());
            if id.get() < i64::MAX as u64 {
                params.append_pair("id_before", &(id.get() + 1).to_string());
            }
        }
        let (headers, bytes) = self.get(url, METADATA_LIMIT).await?;
        let projects: Vec<Project> =
            serde_json::from_slice(&bytes).map_err(|_| AppError::Unavailable)?;
        if next_page(&headers, 1)?.is_some() {
            return Err(AppError::Unavailable);
        }
        match projects.as_slice() {
            [] => Ok(false),
            [project] if project.id == id.get() => Ok(true),
            _ => Err(AppError::Unavailable),
        }
    }
}

fn clone_lookup(path: &str) -> Result<ProjectLookup<'static>, AppError> {
    let escaped = path.to_ascii_lowercase();
    if escaped.contains("%2f") || escaped.contains("%5c") {
        return Err(AppError::InvalidRequest);
    }
    let decoded = percent_decode_str(path.trim_matches('/'))
        .decode_utf8()
        .map_err(|_| AppError::InvalidRequest)?;
    let path = trim_git_suffix(&decoded);
    if !catalog_project_path(path) {
        return Err(AppError::InvalidRequest);
    }
    Ok(ProjectLookup::Path {
        path: Cow::Owned(path.to_owned()),
        search_on_miss: false,
    })
}

fn trim_git_suffix(path: &str) -> &str {
    path.split_at_checked(path.len().saturating_sub(4))
        .filter(|(_, ending)| ending.eq_ignore_ascii_case(".git"))
        .map_or(path, |(path, _)| path)
}

fn numeric_lookup(value: &str) -> Option<ProjectLookup<'static>> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| ProjectLookup::Id(value.parse::<u64>().ok().and_then(|id| id.try_into().ok())))
}

fn catalog_project_path(path: &str) -> bool {
    super::lookup::valid_path(path)
        && path.split('/').all(|segment| segment != "-")
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-' | b'/'))
}

fn apply_view(url: &mut Url, view: ProjectView) {
    let parameter = match view {
        ProjectView::Member => "membership",
        ProjectView::Starred => "starred",
        ProjectView::All => return,
    };
    url.query_pairs_mut().append_pair(parameter, "true");
}

fn project_item(id: ProjectId, path: String) -> Result<CatalogItem, AppError> {
    let path = crate::settings::safe_label(&path, 512)?;
    Ok(CatalogItem {
        id: id.to_string(),
        label: path.clone(),
        path: Some(path),
        board_id: None,
        state: None,
        board_type: None,
        status_ids: Vec::new(),
        estimate_field: None,
        reviewer: None,
        outside_view: None,
    })
}
