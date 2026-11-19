//! Non-secret settings identities shared by provider and persistence adapters.

use crate::identity::ProjectId;
use crate::session::AbsolutePath;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Provider {
    GitLab,
    Jira,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JiraTokenType {
    Scoped,
    Unscoped,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Connection {
    GitLab {
        origin: String,
    },
    Jira {
        site: String,
        email: String,
        token_type: JiraTokenType,
    },
}

impl Connection {
    pub fn provider(&self) -> Provider {
        match self {
            Self::GitLab { .. } => Provider::GitLab,
            Self::Jira { .. } => Provider::Jira,
        }
    }

    pub fn origin(&self) -> &str {
        match self {
            Self::GitLab { origin } => origin,
            Self::Jira { site, .. } => site,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAccount {
    pub connection: Connection,
    pub identity: Identity,
    pub cloud_id: Option<String>,
}

impl VerifiedAccount {
    pub fn same_authority(&self, other: &Self) -> bool {
        self.connection.provider() == other.connection.provider()
            && self.connection.origin() == other.connection.origin()
            && self.identity.id == other.identity.id
            && self.cloud_id == other.cloud_id
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardType {
    Scrum,
    Kanban,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardSelection {
    Selected,
    Any,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardSelectionDraft {
    pub scope_id: String,
    pub board_id: u64,
    pub enabled: bool,
    pub projects: Vec<ProjectId>,
    pub statuses: Vec<u64>,
    pub jira_projects: Vec<String>,
    pub filter: Option<u64>,
    pub estimate_field: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardScope {
    pub selection: BoardSelectionDraft,
    pub label: String,
    pub board_type: BoardType,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DefaultSkills {
    pub codex: Option<NativeSkill>,
    pub claude: Option<NativeSkill>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeSkill {
    pub name: String,
    pub path: AbsolutePath,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutBinding {
    pub root: AbsolutePath,
    pub common_dir: AbsolutePath,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryDraft {
    pub project: ProjectId,
    pub checkout: Option<AbsolutePath>,
    pub board_selection: BoardSelection,
    pub default_skills: DefaultSkills,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryBinding {
    pub project: ProjectId,
    pub path: String,
    pub checkout: Option<CheckoutBinding>,
    pub board_selection: BoardSelection,
    pub default_skills: DefaultSkills,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceDraft {
    pub boards: Vec<BoardSelectionDraft>,
    pub repositories: Vec<RepositoryDraft>,
    pub additional_reviewers: Vec<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceScope {
    pub boards: Vec<BoardScope>,
    pub repositories: Vec<RepositoryBinding>,
    pub additional_reviewers: Vec<u64>,
}

impl WorkspaceScope {
    /// Evaluates the board's explicit selections together with Any tracked board repositories.
    pub fn projects_for(&self, board: &BoardScope) -> impl Iterator<Item = ProjectId> {
        self.repositories
            .iter()
            .filter(move |repo| {
                repo.board_selection == BoardSelection::Any
                    || board.selection.projects.contains(&repo.project)
            })
            .map(|repo| repo.project)
    }
}
