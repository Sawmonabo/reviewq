//! Settings request and sanitized response fields. Tokens are request-only.

use serde::{Deserialize, Serialize};

use crate::{PageCursorDto, ProjectIdDto, ResourceIdDto, RevisionDto, Success};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Provider {
    Gitlab,
    Jira,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TokenType {
    Scoped,
    Unscoped,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GitlabDraft {
    pub origin: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct JiraDraft {
    pub site: String,
    pub email: String,
    pub token_type: TokenType,
}

#[derive(Deserialize)]
#[serde(untagged)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ConnectionDraft {
    Gitlab(GitlabDraft),
    Jira(JiraDraft),
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum TokenInput {
    Keep,
    Replace {
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 8192)))]
        value: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TestConnection {
    pub expected_revision: RevisionDto,
    pub provider: Provider,
    pub connection: ConnectionDraft,
    pub token: TokenInput,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CatalogKind {
    Projects,
    Reviewers,
    #[serde(rename = "reviewerSuggestions")]
    ReviewerSuggestions,
    Boards,
    Board,
    Statuses,
    Filters,
    Sprints,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ProjectView {
    Member,
    Starred,
    All,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CatalogRequest {
    pub expected_revision: RevisionDto,
    pub provider: Provider,
    pub connection: ConnectionDraft,
    pub token: TokenInput,
    pub kind: CatalogKind,
    pub q: Option<String>,
    pub cursor: Option<PageCursorDto>,
    pub board_id: Option<ResourceIdDto>,
    pub view: Option<ProjectView>,
    #[serde(default)]
    pub states: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CheckoutSkillsRequest {
    pub expected_revision: RevisionDto,
    pub gitlab: GitlabDraft,
    pub token: TokenInput,
    pub project_id: ProjectIdDto,
    pub checkout: String,
    pub provider: crate::HostProviderDto,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CheckoutSkills {
    pub settings_revision: RevisionDto,
    pub project_id: ProjectIdDto,
    pub checkout: String,
    pub provider: crate::HostProviderDto,
    pub skills: Vec<Skill>,
}

pub type CheckoutSkillsResponse = Success<CheckoutSkills>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Disconnect {
    pub expected_revision: RevisionDto,
    pub provider: Provider,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SaveSettings {
    pub expected_revision: RevisionDto,
    pub draft: SettingsDraft,
    pub tokens: TokenChanges,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SettingsDraft {
    pub gitlab: GitlabDraft,
    pub jira: JiraDraft,
    pub boards: Vec<BoardScope>,
    pub repositories: Vec<RepositoryBinding>,
    pub additional_reviewer_ids: Vec<ResourceIdDto>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TokenChanges {
    pub gitlab: TokenInput,
    pub jira: TokenInput,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BoardScope {
    pub scope_id: ResourceIdDto,
    pub board_id: ResourceIdDto,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: BoardType,
    pub enabled: bool,
    pub project_ids: Vec<ProjectIdDto>,
    pub review_status_ids: Vec<ResourceIdDto>,
    pub jira_project_keys: Vec<String>,
    pub team_filter_id: Option<ResourceIdDto>,
    pub story_points_field: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum BoardType {
    Scrum,
    Kanban,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RepositoryBinding {
    pub project_id: ProjectIdDto,
    pub path: String,
    pub checkout: Option<String>,
    pub board_selection: BoardSelection,
    pub default_skills: DefaultSkills,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum BoardSelection {
    Selected,
    Any,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DefaultSkills {
    pub codex: Option<Skill>,
    pub claude: Option<Skill>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Skill {
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Identity {
    pub id: ResourceIdDto,
    pub label: String,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VerifiedConnection {
    pub provider: Provider,
    pub identity: Identity,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CatalogItem {
    pub id: ResourceIdDto,
    pub label: String,
    pub path: Option<String>,
    pub board_id: Option<ResourceIdDto>,
    pub state: Option<String>,
    pub board_type: Option<BoardType>,
    pub status_ids: Vec<ResourceIdDto>,
    pub estimate_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<CatalogReviewer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outside_view: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CatalogReviewer {
    pub username: String,
    pub public_email: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CatalogResult {
    pub items: Vec<CatalogItem>,
    pub next_cursor: Option<PageCursorDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SettingsView {
    pub settings_revision: RevisionDto,
    pub mode: Mode,
    pub connections: Connections,
    pub boards: Vec<BoardScope>,
    pub repositories: Vec<RepositoryBinding>,
    pub additional_reviewer_ids: Vec<ResourceIdDto>,
    pub chat_setup: ChatSetup,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Mode {
    Setup,
    Connected,
    Disconnected,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Connections {
    pub gitlab: GitlabConnection,
    pub jira: JiraConnection,
}

#[derive(Default, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GitlabConnection {
    pub origin: Option<String>,
    pub configured: bool,
    pub identity: Option<Identity>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct JiraConnection {
    pub site: Option<String>,
    pub email: Option<String>,
    pub token_type: Option<TokenType>,
    pub configured: bool,
    pub identity: Option<Identity>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChatSetup {
    pub state: ChatSetupState,
    pub issues: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ChatSetupState {
    Ready,
    Pending,
    CleanupConflict,
}

pub type SettingsResponse = Success<SettingsView>;
pub type VerifiedConnectionResponse = Success<VerifiedConnection>;
pub type CatalogResponse = Success<CatalogResult>;
