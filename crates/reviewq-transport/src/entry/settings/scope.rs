use reviewq_app::access::AppError;
use reviewq_domain::identity::ProjectId;
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings as domain;
use reviewq_protocol::settings as dto;
use reviewq_protocol::{ProjectIdDto, ResourceIdDto};

use super::{parse, resource};

pub(super) fn draft(
    boards: Vec<dto::BoardScope>,
    repositories: Vec<dto::RepositoryBinding>,
    additional_reviewers: Vec<ResourceIdDto>,
) -> Result<domain::WorkspaceDraft, AppError> {
    Ok(domain::WorkspaceDraft {
        boards: boards
            .into_iter()
            .map(board_draft)
            .collect::<Result<_, _>>()?,
        repositories: repositories
            .into_iter()
            .map(repo_draft)
            .collect::<Result<_, _>>()?,
        additional_reviewers: numbers(additional_reviewers)?,
    })
}

fn board_draft(value: dto::BoardScope) -> Result<domain::BoardSelectionDraft, AppError> {
    Ok(domain::BoardSelectionDraft {
        scope_id: value.scope_id.as_str().to_owned(),
        board_id: parse(value.board_id.as_str())?,
        enabled: value.enabled,
        projects: value
            .project_ids
            .into_iter()
            .map(project)
            .collect::<Result<_, _>>()?,
        statuses: numbers(value.review_status_ids)?,
        jira_projects: value.jira_project_keys,
        filter: value
            .team_filter_id
            .map(|value| parse(value.as_str()))
            .transpose()?,
        estimate_field: value.story_points_field,
    })
}

fn repo_draft(value: dto::RepositoryBinding) -> Result<domain::RepositoryDraft, AppError> {
    Ok(domain::RepositoryDraft {
        project: project(value.project_id)?,
        checkout: value.checkout.map(path).transpose()?,
        board_selection: match value.board_selection {
            dto::BoardSelection::Selected => domain::BoardSelection::Selected,
            dto::BoardSelection::Any => domain::BoardSelection::Any,
        },
        default_skills: domain::DefaultSkills {
            codex: value.default_skills.codex.map(skill_draft).transpose()?,
            claude: value.default_skills.claude.map(skill_draft).transpose()?,
        },
    })
}

pub(super) fn path(value: String) -> Result<AbsolutePath, AppError> {
    if value.len() > 4096 || value.contains('\0') {
        return Err(AppError::InvalidRequest);
    }
    AbsolutePath::from_canonical(value.into()).map_err(|_| AppError::InvalidRequest)
}

fn skill_draft(value: dto::Skill) -> Result<domain::NativeSkill, AppError> {
    if value.name.is_empty() || value.name.len() > 256 || value.name.chars().any(char::is_control) {
        return Err(AppError::InvalidRequest);
    }
    Ok(domain::NativeSkill {
        name: value.name,
        path: path(value.path)?,
    })
}

fn project(value: ProjectIdDto) -> Result<ProjectId, AppError> {
    ProjectId::try_from(value.get()).map_err(|_| AppError::InvalidRequest)
}

fn numbers(values: Vec<ResourceIdDto>) -> Result<Vec<u64>, AppError> {
    values
        .into_iter()
        .map(|value| parse(value.as_str()))
        .collect()
}

pub(super) fn board(value: domain::BoardScope) -> Result<dto::BoardScope, AppError> {
    let selection = value.selection;
    Ok(dto::BoardScope {
        scope_id: resource(selection.scope_id)?,
        board_id: resource(selection.board_id.to_string())?,
        name: value.label,
        kind: match value.board_type {
            domain::BoardType::Scrum => dto::BoardType::Scrum,
            domain::BoardType::Kanban => dto::BoardType::Kanban,
        },
        enabled: selection.enabled,
        project_ids: selection
            .projects
            .into_iter()
            .map(project_dto)
            .collect::<Result<_, _>>()?,
        review_status_ids: number_dtos(selection.statuses)?,
        jira_project_keys: selection.jira_projects,
        team_filter_id: selection
            .filter
            .map(|id| resource(id.to_string()))
            .transpose()?,
        story_points_field: selection.estimate_field,
    })
}

pub(super) fn repo(value: domain::RepositoryBinding) -> Result<dto::RepositoryBinding, AppError> {
    Ok(dto::RepositoryBinding {
        project_id: project_dto(value.project)?,
        path: value.path,
        checkout: value
            .checkout
            .map(|binding| path_text(&binding.root))
            .transpose()?,
        board_selection: match value.board_selection {
            domain::BoardSelection::Selected => dto::BoardSelection::Selected,
            domain::BoardSelection::Any => dto::BoardSelection::Any,
        },
        default_skills: dto::DefaultSkills {
            codex: value.default_skills.codex.map(skill).transpose()?,
            claude: value.default_skills.claude.map(skill).transpose()?,
        },
    })
}

pub(super) fn skill(value: domain::NativeSkill) -> Result<dto::Skill, AppError> {
    Ok(dto::Skill {
        name: value.name,
        path: path_text(&value.path)?,
    })
}

fn path_text(value: &AbsolutePath) -> Result<String, AppError> {
    value
        .as_path()
        .to_str()
        .map(str::to_owned)
        .ok_or(AppError::Internal)
}

fn project_dto(value: ProjectId) -> Result<ProjectIdDto, AppError> {
    ProjectIdDto::new(value.get()).ok_or(AppError::Internal)
}

pub(super) fn number_dtos(values: Vec<u64>) -> Result<Vec<ResourceIdDto>, AppError> {
    values
        .into_iter()
        .map(|id| resource(id.to_string()))
        .collect()
}
