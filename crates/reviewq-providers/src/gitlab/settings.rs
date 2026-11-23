use std::collections::HashSet;

use reviewq_app::access::AppError;
use reviewq_app::settings::{CatalogItem, CatalogKind, CatalogPage, CatalogQuery, ProjectView};
use reviewq_domain::identity::ProjectId;
use reviewq_domain::settings::Identity;
use serde::Deserialize;

use super::{GitLab, METADATA_LIMIT, Project};

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
        let page = query.offset + 1;
        let mut url = self.url("api/v4/projects")?;
        {
            let mut params = url.query_pairs_mut();
            params
                .append_pair("page", &page.to_string())
                .append_pair("per_page", "100")
                .append_pair("simple", "true");
            match view {
                ProjectView::Member => {
                    params.append_pair("membership", "true");
                }
                ProjectView::Starred => {
                    params.append_pair("starred", "true");
                }
                ProjectView::All => {}
            }
            if let Some(text) = query.text {
                params.append_pair("search", &text);
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
                let path = crate::settings::safe_label(&project.path_with_namespace, 512)?;
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
                })
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
}
