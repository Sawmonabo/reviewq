//! Current project membership and the authority epochs of managed checkouts.

use reviewq_domain::identity::{GitLabOrigin, GrantId, ProjectId, Revision};
use reviewq_domain::session::AbsolutePath;

use crate::access::{Actor, AppFuture};
use crate::events::Snapshot;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ConfiguredProject {
    pub origin: GitLabOrigin,
    pub project_id: ProjectId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantBinding {
    pub id: GrantId,
    pub issuing_project: ConfiguredProject,
    pub checkout: AbsolutePath,
    pub common_dir: AbsolutePath,
    pub epoch: Revision,
    pub verifier: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceConfig {
    pub projects: Vec<ConfiguredProject>,
    pub grants: Vec<GrantBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceContext {
    pub revision: Revision,
    pub projects: Vec<ConfiguredProject>,
}

#[derive(Clone, Debug)]
pub struct IssueGrant {
    pub project: ConfiguredProject,
    pub checkout: AbsolutePath,
    pub common_dir: AbsolutePath,
    pub verifier: [u8; 32],
}

pub trait WorkspaceStore: Send + Sync {
    fn workspace_context(&self, actor: Actor) -> AppFuture<'_, Snapshot<WorkspaceContext>>;
    fn authenticate_grant(&self, verifier: [u8; 32]) -> AppFuture<'_, Actor>;
    fn issue_grant(&self, actor: Actor, input: IssueGrant)
    -> AppFuture<'_, Snapshot<GrantBinding>>;
    fn revoke_grant(&self, actor: Actor, common_dir: AbsolutePath)
    -> AppFuture<'_, Snapshot<bool>>;

    /// Replaces active memberships and checkout authority under one settings revision.
    /// A revoked or rotated grant cannot be reactivated at its older epoch.
    fn replace_workspace(
        &self,
        actor: Actor,
        expected: Revision,
        input: WorkspaceConfig,
    ) -> AppFuture<'_, Snapshot<WorkspaceContext>>;
}
