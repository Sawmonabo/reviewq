use std::collections::HashSet;

use reviewq_app::access::AppError;
use reviewq_app::settings::SavedConnection;
use reviewq_app::sources::ReviewSourceReader;
use reviewq_domain::settings::{
    BoardScope, BoardType, Connection, MAX_CONFIGURED_BOARDS, MAX_CONFIGURED_REPOSITORIES,
    RepositoryBinding, WorkspaceDraft, WorkspaceScope,
};

use crate::checkout;
use crate::jira::{Jira, valid_estimate_field, valid_project};

use super::SourceAccounts;

impl SourceAccounts {
    pub(super) async fn validate_workspace(
        &self,
        gitlab: &SavedConnection,
        jira: &SavedConnection,
        draft: WorkspaceDraft,
    ) -> Result<WorkspaceScope, AppError> {
        let WorkspaceDraft {
            boards,
            repositories,
            additional_reviewers,
        } = draft;
        if repositories.len() > MAX_CONFIGURED_REPOSITORIES
            || boards.len() > MAX_CONFIGURED_BOARDS
            || additional_reviewers.len() > 256
            || additional_reviewers.iter().any(|id| !valid_id(*id))
            || !unique(additional_reviewers.iter().copied())
            || !unique(repositories.iter().map(|repo| repo.project))
            || !unique(boards.iter().map(|board| board.board_id))
            || !unique(boards.iter().map(|board| board.scope_id.as_str()))
        {
            return Err(AppError::InvalidRequest);
        }
        let configured = repositories
            .iter()
            .map(|repo| repo.project)
            .collect::<HashSet<_>>();
        for board in &boards {
            if !valid_id(board.board_id)
                || board.scope_id.is_empty()
                || board.scope_id.len() > 512
                || board.scope_id.chars().any(char::is_control)
                || board
                    .projects
                    .iter()
                    .any(|project| !configured.contains(project))
                || !unique(board.projects.iter().copied())
                || board.statuses.len() > 100
                || board.statuses.iter().any(|status| !valid_id(*status))
                || !unique(board.statuses.iter().copied())
                || board.jira_projects.len() > 100
                || !board.jira_projects.iter().all(|key| valid_project(key))
                || !unique(board.jira_projects.iter())
                || board.filter.is_some_and(|id| !valid_id(id))
                || board
                    .estimate_field
                    .as_deref()
                    .is_some_and(|field| !valid_estimate_field(field))
            {
                return Err(AppError::InvalidRequest);
            }
        }
        let Connection::GitLab { origin } = &gitlab.account.connection else {
            return Err(AppError::InvalidRequest);
        };
        let gitlab = self.gitlab(
            origin,
            gitlab.token.as_ref().ok_or(AppError::PreconditionFailed)?,
        )?;
        let mut scope = WorkspaceScope {
            additional_reviewers,
            ..WorkspaceScope::default()
        };
        for repo in repositories {
            let path = gitlab.verify_project(repo.project).await?;
            let checkout = match repo.checkout {
                Some(path) => Some(checked_checkout(&gitlab, repo.project, &path).await?),
                None if repo.default_skills.codex.is_none()
                    && repo.default_skills.claude.is_none() =>
                {
                    None
                }
                None => return Err(AppError::InvalidRequest),
            };
            scope.repositories.push(RepositoryBinding {
                project: repo.project,
                path,
                checkout,
                board_selection: repo.board_selection,
                default_skills: repo.default_skills,
            });
        }
        if boards.is_empty() {
            return Ok(scope);
        }
        let cloud = jira.account.cloud_id.as_deref().ok_or(AppError::Conflict)?;
        let jira = Jira::for_bound(
            self.network.clone(),
            &jira.account.connection,
            jira.token.as_ref().ok_or(AppError::PreconditionFailed)?,
            cloud,
            self.fixture,
            None,
        )?;
        let mut global_statuses = None;
        for mut board in boards {
            let metadata = jira.board(board.board_id).await?;
            let board_type = match metadata.board_type.as_deref() {
                Some("scrum") => BoardType::Scrum,
                Some("kanban") => BoardType::Kanban,
                _ => return Err(AppError::Unavailable),
            };
            let allowed = if metadata.status_ids.is_empty() && !board.statuses.is_empty() {
                if global_statuses.is_none() {
                    global_statuses = Some(jira.status_ids().await?);
                }
                global_statuses.as_ref().ok_or(AppError::Internal)?.clone()
            } else {
                metadata.status_ids.into_iter().collect()
            };
            if board
                .statuses
                .iter()
                .any(|status| !allowed.contains(&status.to_string()))
            {
                return Err(AppError::InvalidRequest);
            }
            if let Some(filter) = board.filter {
                jira.verify_filter(filter).await?;
            }
            if board.estimate_field.is_none() {
                board.estimate_field = metadata.estimate_field;
            }
            let board = BoardScope {
                selection: board,
                label: metadata.label,
                board_type,
            };
            if board.selection.enabled {
                if board.selection.statuses.is_empty()
                    || scope.projects_for(&board).next().is_none()
                {
                    return Err(AppError::InvalidRequest);
                }
                jira.issue_page(&board, None, None, 1).await?;
            }
            scope.boards.push(board);
        }
        Ok(scope)
    }
}

pub(super) async fn checked_checkout(
    source: &crate::GitLab,
    project: reviewq_domain::identity::ProjectId,
    path: &reviewq_domain::session::AbsolutePath,
) -> Result<reviewq_domain::settings::CheckoutBinding, AppError> {
    let (checked, remote) = checkout::verify_checkout(path).await?;
    source
        .verify_checkout(source.configured_project(project), remote)
        .await?;
    Ok(checked)
}

fn valid_id(id: u64) -> bool {
    (1..=i64::MAX as u64).contains(&id)
}

fn unique<T: Eq + std::hash::Hash>(items: impl Iterator<Item = T>) -> bool {
    let mut seen = HashSet::new();
    items.into_iter().all(|value| seen.insert(value))
}
