use std::os::unix::ffi::OsStrExt;

use reviewq_app::access::AppError;
use reviewq_domain::identity::ProjectId;
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::{
    BoardScope, BoardSelection, BoardSelectionDraft, BoardType, CheckoutBinding, DefaultSkills,
    NativeSkill, RepositoryBinding, WorkspaceScope,
};
use rusqlite::{Connection, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{StoreError, queue::path};

pub(super) fn read(db: &Connection) -> Result<WorkspaceScope, StoreError> {
    let mut query = db.prepare_cached(
        "SELECT project_id,path,checkout,common_dir,board_selection,
            codex_skill,codex_skill_path,claude_skill,claude_skill_path
         FROM settings_repositories ORDER BY project_id",
    )?;
    let mut rows = query.query([])?;
    let mut repositories = Vec::new();
    while let Some(row) = rows.next()? {
        let checkout = match (
            row.get::<_, Option<Vec<u8>>>(2)?,
            row.get::<_, Option<Vec<u8>>>(3)?,
        ) {
            (Some(root), Some(common_dir)) => Some(CheckoutBinding {
                root: path(root)?,
                common_dir: path(common_dir)?,
            }),
            (None, None) => None,
            _ => return Err(AppError::Internal.into()),
        };
        repositories.push(RepositoryBinding {
            project: project(row.get(0)?)?,
            path: row.get(1)?,
            checkout,
            board_selection: match row.get::<_, String>(4)?.as_str() {
                "selected" => BoardSelection::Selected,
                "any" => BoardSelection::Any,
                _ => return Err(AppError::Internal.into()),
            },
            default_skills: DefaultSkills {
                codex: read_skill(row, 5)?,
                claude: read_skill(row, 7)?,
            },
        });
    }
    let mut query = db.prepare_cached(
        "SELECT scope_id,board_id,label,kind,enabled,projects,statuses,jira_projects,
            filter_id,estimate_field FROM settings_boards ORDER BY board_id",
    )?;
    let mut rows = query.query([])?;
    let mut boards = Vec::new();
    while let Some(row) = rows.next()? {
        let projects = decode::<Vec<i64>>(row, 5)?
            .into_iter()
            .map(project)
            .collect::<Result<Vec<_>, _>>()?;
        boards.push(BoardScope {
            label: row.get(2)?,
            board_type: match row.get::<_, String>(3)?.as_str() {
                "scrum" => BoardType::Scrum,
                "kanban" => BoardType::Kanban,
                _ => return Err(AppError::Internal.into()),
            },
            selection: BoardSelectionDraft {
                scope_id: row.get(0)?,
                board_id: positive(row.get(1)?)?,
                enabled: row.get(4)?,
                projects,
                statuses: decode(row, 6)?,
                jira_projects: decode(row, 7)?,
                filter: row.get::<_, Option<i64>>(8)?.map(positive).transpose()?,
                estimate_field: row.get(9)?,
            },
        });
    }
    let additional_reviewers = db
        .prepare_cached("SELECT reviewer_id FROM settings_reviewers ORDER BY reviewer_id")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .map(|id| positive(id?))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorkspaceScope {
        boards,
        repositories,
        additional_reviewers,
    })
}

pub(super) fn replace(
    db: &Connection,
    origin: &str,
    scope: WorkspaceScope,
) -> Result<(), StoreError> {
    db.execute_batch(
        "DELETE FROM settings_repositories;
         DELETE FROM settings_boards;
         DELETE FROM settings_reviewers;
         DELETE FROM configured_projects;",
    )?;
    for repo in scope.repositories {
        let id = repo.project.as_sqlite_integer();
        let (checkout, common) = repo.checkout.as_ref().map_or((None, None), |binding| {
            (Some(bytes(&binding.root)), Some(bytes(&binding.common_dir)))
        });
        let (codex, codex_path) = skill_columns(repo.default_skills.codex.as_ref());
        let (claude, claude_path) = skill_columns(repo.default_skills.claude.as_ref());
        let selection = match repo.board_selection {
            BoardSelection::Selected => "selected",
            BoardSelection::Any => "any",
        };
        db.execute(
            "INSERT INTO settings_repositories VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                id,
                repo.path,
                checkout,
                common,
                selection,
                codex,
                codex_path,
                claude,
                claude_path
            ],
        )?;
        db.execute(
            "INSERT INTO configured_projects VALUES (?1,?2)",
            params![origin, id],
        )?;
    }
    for board in scope.boards {
        let selection = board.selection;
        let projects = selection
            .projects
            .into_iter()
            .map(ProjectId::as_sqlite_integer)
            .collect::<Vec<_>>();
        let kind = match board.board_type {
            BoardType::Scrum => "scrum",
            BoardType::Kanban => "kanban",
        };
        db.execute(
            "INSERT INTO settings_boards VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                selection.scope_id,
                integer(selection.board_id)?,
                board.label,
                kind,
                selection.enabled,
                encode(&projects)?,
                encode(&selection.statuses)?,
                encode(&selection.jira_projects)?,
                selection.filter.map(integer).transpose()?,
                selection.estimate_field,
            ],
        )?;
    }
    for reviewer in scope.additional_reviewers {
        db.execute(
            "INSERT INTO settings_reviewers VALUES (?1)",
            [integer(reviewer)?],
        )?;
    }
    db.execute(
        "UPDATE repo_grants AS g SET active=0 WHERE active=1 AND (
            origin<>?1 OR NOT EXISTS (SELECT 1 FROM settings_repositories AS r
             WHERE r.project_id=g.project_id AND r.common_dir=g.common_dir AND r.checkout=g.checkout))",
        [origin],
    )?;
    super::projection::set_desired(db)?;
    Ok(())
}

pub(super) fn discovery_changed(
    db: &Connection,
    next: &WorkspaceScope,
) -> Result<bool, StoreError> {
    let previous = read(db)?;
    Ok(discovery(&previous) != discovery(next))
}

fn discovery(scope: &WorkspaceScope) -> Vec<(BoardSelectionDraft, BoardType)> {
    let mut selection: Vec<_> = scope
        .boards
        .iter()
        .filter(|board| board.selection.enabled)
        .map(|board| {
            let mut terms = board.selection.clone();
            terms.projects = scope.projects_for(board).collect();
            terms.projects.sort_unstable();
            terms.statuses.sort_unstable();
            terms.jira_projects.sort_unstable();
            (terms, board.board_type)
        })
        .collect();
    selection.sort_unstable_by_key(|item| item.0.board_id);
    selection
}

fn read_skill(row: &Row<'_>, col: usize) -> Result<Option<NativeSkill>, StoreError> {
    match (row.get(col)?, row.get(col + 1)?) {
        (Some(name), Some(bytes)) => Ok(Some(NativeSkill {
            name,
            path: path(bytes)?,
        })),
        (None, None) => Ok(None),
        _ => Err(AppError::Internal.into()),
    }
}

fn skill_columns(skill: Option<&NativeSkill>) -> (Option<&str>, Option<&[u8]>) {
    skill.map_or((None, None), |skill| {
        (Some(&skill.name), Some(bytes(&skill.path)))
    })
}

fn bytes(path: &AbsolutePath) -> &[u8] {
    path.as_path().as_os_str().as_bytes()
}

fn project(id: i64) -> Result<ProjectId, StoreError> {
    ProjectId::try_from(id).map_err(|_| AppError::Internal.into())
}

fn positive(id: i64) -> Result<u64, StoreError> {
    u64::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(AppError::Internal.into())
}

fn integer(id: u64) -> Result<i64, StoreError> {
    i64::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .ok_or(AppError::InvalidRequest.into())
}

fn encode(value: &impl Serialize) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|_| AppError::Internal.into())
}

fn decode<T: DeserializeOwned>(row: &Row<'_>, col: usize) -> Result<T, StoreError> {
    serde_json::from_str(&row.get::<_, String>(col)?).map_err(|_| AppError::Internal.into())
}
