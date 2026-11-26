use reviewq_domain::identity::{MrIdentity, ReviewId, Revision};
use reviewq_domain::session::LeadTarget;

use crate::access::{Actor, AppError};
use crate::events::Snapshot;
use crate::queue::{ReviewLookup, ReviewSource};
use crate::workspace::WorkspaceStore;

use super::{HostProof, LeadStore, RegisterLead};

pub async fn authorize_registration(
    store: &(impl ReviewLookup + WorkspaceStore + ?Sized),
    actor: Actor,
    review: ReviewId,
) -> Result<(Revision, MrIdentity), AppError> {
    let (revision, source) = store.authorized_source(actor.clone(), review).await?.value;
    let mr = source.identity;
    let workspace = store.workspace_context(actor).await?.value;
    if revision != workspace.revision {
        return Err(AppError::StaleRevision);
    }
    if !workspace
        .projects
        .iter()
        .any(|project| project.origin == *mr.origin() && project.project_id == mr.project_id())
    {
        return Err(AppError::NotFound);
    }
    Ok((revision, mr))
}

/// One binding operation for operator selection and host-issued terminal links.
pub async fn register_proven(
    store: &(impl LeadStore + ?Sized),
    proof: HostProof,
    review: ReviewSource,
    expected_settings: Revision,
) -> Result<Snapshot<LeadTarget>, AppError> {
    let identity = proof.binding.identity();
    let session_key = identity
        .provider()
        .session_key(identity.native_id())
        .map_err(|_| AppError::InvalidRequest)?;
    store
        .register_lead(
            Actor::native_session(identity.clone()),
            RegisterLead {
                expected_settings,
                review,
                provider: identity.provider(),
                profile: identity.profile().clone(),
                native_id: identity.native_id().clone(),
                session_key,
                cwd: proof.binding.cwd().clone(),
                native_source: proof.source,
                activity: proof.activity,
                metadata: proof.metadata,
            },
        )
        .await
}
