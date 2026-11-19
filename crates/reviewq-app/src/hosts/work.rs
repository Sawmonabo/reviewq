use std::sync::Arc;

use reviewq_domain::identity::{GitSha, RunId, SessionKey, SubmissionId};

use crate::access::{Actor, AppError};
use crate::events::Snapshot;
use crate::settings::RuntimeSources;

use super::{
    DeliveryClaim, DeliveryReceipt, DeliveryWork, LeadStore, MessageBody, NativeHost,
    RecoverableDelivery, SubmitMessage,
};

/// The dispatch adapter must call this after preparing the exact native request.
pub struct DispatchGuard {
    store: Arc<dyn LeadStore>,
    claim: DeliveryClaim,
    workflow_confirmed: bool,
    sources: Arc<RuntimeSources>,
}

impl DispatchGuard {
    pub fn workflow(&self) -> Option<&crate::workflow::WorkflowGuard> {
        match &self.claim.work {
            DeliveryWork::Dispatch { workflow, .. } => workflow.as_deref(),
            DeliveryWork::Reconcile { .. } => None,
        }
    }

    pub async fn workflow_requirements(
        &self,
    ) -> Result<crate::workflow::WorkflowRequirements, AppError> {
        self.store.workflow_requirements(&self.claim).await
    }

    /// Uses the current credential generation to observe the actual remote workflow requirements.
    pub async fn verify_workflow_source(&self) -> Result<GitSha, AppError> {
        let guard = self.workflow().ok_or(AppError::InvalidRequest)?;
        let views = self.sources.current().await?;
        if views.revision != guard.settings || !views.collection_enabled {
            return Err(AppError::StaleRevision);
        }
        let reader = views.reader.ok_or(AppError::Unavailable)?;
        let requirements = self.workflow_requirements().await?;
        let snapshot = reader
            .workflow_snapshot(requirements.identity.clone())
            .await?;
        requirements.check(&snapshot, &guard.head)?;
        Ok(snapshot.review.head_sha)
    }

    /// Records successful revalidation by the adapter immediately before automatic delivery.
    pub fn confirm_workflow(
        mut self,
        settings: &crate::workflow::ReadyWorkflow,
        head: &reviewq_domain::identity::GitSha,
    ) -> Result<Self, AppError> {
        let guard = self.workflow().ok_or(AppError::InvalidRequest)?;
        if &guard.ready != settings || &guard.head != head {
            return Err(AppError::StaleRevision);
        }
        self.workflow_confirmed = true;
        Ok(self)
    }

    pub async fn check(self) -> Result<(), AppError> {
        if self.workflow().is_some() && !self.workflow_confirmed {
            return Err(AppError::Forbidden);
        }
        self.store.authorize_dispatch(self.claim).await
    }
}

pub async fn send_message(
    store: &(impl LeadStore + ?Sized),
    actor: Actor,
    run: RunId,
    session: SessionKey,
    submission: SubmissionId,
    message: MessageBody,
) -> Result<Snapshot<DeliveryReceipt>, AppError> {
    let target = store.lookup_lead(actor.clone(), run, session).await?.value;
    store
        .admit_message(
            actor,
            SubmitMessage::user_input(target, submission, message, None),
        )
        .await
}

pub async fn recover_delivery(
    store: Arc<dyn LeadStore>,
    host: &dyn NativeHost,
    candidate: RecoverableDelivery,
    sources: Arc<RuntimeSources>,
) -> Result<(), AppError> {
    let Some(claim) = store
        .claim_delivery(candidate.cursor.delivery_id, candidate.state)
        .await?
    else {
        return Ok(());
    };
    let result = match &claim.work {
        DeliveryWork::Dispatch {
            text, report_ref, ..
        } => {
            host.dispatch(
                claim.target.clone(),
                claim.delivery_id.clone(),
                text.clone(),
                report_ref.clone(),
                DispatchGuard {
                    store: store.clone(),
                    claim: claim.clone(),
                    workflow_confirmed: false,
                    sources,
                },
            )
            .await
        }
        DeliveryWork::Reconcile { cursor } => {
            host.observe(
                claim.target.clone(),
                claim.delivery_id.clone(),
                cursor.clone(),
                claim.purpose.requires_new_turn(),
            )
            .await
        }
    };
    store.finish_delivery(claim, result).await?;
    Ok(())
}

pub async fn receipt(
    store: &(impl LeadStore + ?Sized),
    actor: Actor,
    run: RunId,
    session: SessionKey,
    submission: SubmissionId,
) -> Result<Snapshot<DeliveryReceipt>, AppError> {
    let lead = store.lookup_lead(actor.clone(), run, session).await?.value;
    store
        .read_receipt(actor, lead.target().clone(), submission)
        .await
}
