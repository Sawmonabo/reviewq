//! Current project membership and the authority epochs of managed checkouts.

use reviewq_domain::identity::{GitLabOrigin, GrantId, ProjectId, Revision};
use reviewq_domain::session::AbsolutePath;
use reviewq_domain::settings::CheckoutBinding;

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
    pub settings: Revision,
    pub project: ConfiguredProject,
    pub checkout: AbsolutePath,
    pub common_dir: AbsolutePath,
    pub verifier: [u8; 32],
    pub operator_requested: bool,
}

pub struct GrantAddress {
    pub settings: Revision,
    pub project: ProjectId,
    pub checkout: AbsolutePath,
    pub common_dir: AbsolutePath,
    pub verifier: [u8; 32],
}

#[derive(Clone)]
pub struct CheckoutProjection {
    pub settings: Revision,
    pub generation: Revision,
    pub project: ConfiguredProject,
    pub checkout: CheckoutBinding,
    pub desired: bool,
    pub ready: bool,
}

pub enum ProjectionOutcome {
    Applied {
        verifier: [u8; 32],
        topology: [u8; 32],
    },
    Removed,
    Deferred(String),
}

pub trait RepositoryChatProjectionStore: Send + Sync {
    fn checkout_projections(
        &self,
        after: Option<AbsolutePath>,
    ) -> AppFuture<'_, Vec<CheckoutProjection>>;
    fn check_checkout_projection(&self, job: CheckoutProjection) -> AppFuture<'_, ()>;
    fn finish_checkout_projection(
        &self,
        job: CheckoutProjection,
        outcome: ProjectionOutcome,
    ) -> AppFuture<'_, ()>;
}

pub trait WorkspaceStore: Send + Sync {
    fn workspace_context(&self, actor: Actor) -> AppFuture<'_, Snapshot<WorkspaceContext>>;
    fn authenticate_grant(&self, verifier: [u8; 32]) -> AppFuture<'_, Actor>;
    fn granted(
        &self,
        actor: Actor,
        expected: GrantAddress,
    ) -> AppFuture<'_, Snapshot<GrantBinding>>;
    fn confirm_grant_projection(
        &self,
        actor: Actor,
        expected: GrantAddress,
    ) -> AppFuture<'_, Snapshot<GrantBinding>>;
    /// Whether `verifier` is the key of a withdrawn grant, which is never issued again, so the
    /// checkout holding it must replace it before asking for a grant.
    fn retired_grant(&self, actor: Actor, verifier: [u8; 32]) -> AppFuture<'_, Snapshot<bool>>;
    fn issue_grant(&self, actor: Actor, input: IssueGrant)
    -> AppFuture<'_, Snapshot<GrantBinding>>;
    fn revoke_grant(&self, actor: Actor, common_dir: AbsolutePath)
    -> AppFuture<'_, Snapshot<bool>>;
    fn request_checkout_projection(
        &self,
        actor: Actor,
        settings: Revision,
        common_dir: AbsolutePath,
        topology: [u8; 32],
    ) -> AppFuture<'_, Snapshot<bool>>;

    /// Replaces active memberships and checkout authority under one settings revision.
    /// A revoked or rotated grant cannot be reactivated at its older epoch.
    fn replace_workspace(
        &self,
        actor: Actor,
        expected: Revision,
        input: WorkspaceConfig,
    ) -> AppFuture<'_, Snapshot<WorkspaceContext>>;
}
