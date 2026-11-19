use crate::access::{AppError, AppFuture};
use crate::hosts::MessageBody;
use crate::sources::ReviewSourceReader;
use crate::workflow::{NativeWorkflowProof, ReadyWorkflow, WorkflowHold, WorkflowReadiness};

use super::publication::publication_observation;
use super::{ActionCommandStore, ActionWork, Approval, Evidence, PublicationEvidence};

pub struct PublicationRetry {
    pub ready: ReadyWorkflow,
    pub prompt: MessageBody,
}

pub trait PublicationWorkerStore: ActionCommandStore {
    fn due_publications(&self, limit: u16) -> AppFuture<'_, Vec<ActionWork>>;
    fn defer_publication(&self, work: ActionWork, reason: &'static str) -> AppFuture<'_, ()>;
    fn reserve_publication(
        &self,
        work: ActionWork,
        evidence: PublicationEvidence,
        retry: PublicationRetry,
    ) -> AppFuture<'_, ()>;
}

pub async fn run_due_publications(
    store: &dyn PublicationWorkerStore,
    sources: &dyn ReviewSourceReader,
    native: &dyn NativeWorkflowProof,
) -> Result<(), AppError> {
    for work in store.due_publications(4).await? {
        let result = check(store, sources, native, work).await;
        if let Err(error) = result
            && !matches!(
                error,
                AppError::StaleRevision | AppError::Conflict | AppError::Busy
            )
        {
            return Err(error);
        }
    }
    Ok(())
}

async fn check(
    store: &dyn PublicationWorkerStore,
    sources: &dyn ReviewSourceReader,
    native: &dyn NativeWorkflowProof,
    work: ActionWork,
) -> Result<(), AppError> {
    let source = match sources.workflow_snapshot(work.identity.clone()).await {
        Ok(source) => source,
        Err(_) => {
            return store
                .defer_publication(work, "publication_evidence_unavailable")
                .await;
        }
    };
    if source.review.identity != work.identity
        || source.authenticated_actor != work.authenticated_actor
    {
        return store
            .defer_publication(work, "publication_reviewer_changed")
            .await;
    }
    let evidence = publication_observation(&work, source);
    if evidence.open
        && Some(&evidence.auth_head) == work.action.reviewed_sha.as_ref()
        && work.action.pending_submission.is_none()
        && work.action.attempts < 3
        && evidence.approval != Approval::Unknown
        && evidence
            .targets
            .iter()
            .all(|target| target.state != Evidence::Unknown)
        && (evidence.approval == Approval::Absent
            || evidence
                .targets
                .iter()
                .any(|target| target.state == Evidence::Absent))
    {
        match native.dispatch_readiness(work.target.clone()).await {
            Ok(WorkflowReadiness::Ready(ready)) => {
                if ready.latest_user_entry_id != work.action.request_entry
                    || ready.latest_user_turn_id != work.action.turn
                {
                    store.record_action_evidence(work, evidence, true).await?;
                    return Ok(());
                }
                if let Some(prompt) = retry_prompt(&work, &evidence) {
                    return store
                        .reserve_publication(work, evidence, PublicationRetry { ready, prompt })
                        .await;
                }
            }
            Ok(WorkflowReadiness::Held(WorkflowHold::ActiveWork)) => {
                return store
                    .defer_publication(work, "publication_native_work_active")
                    .await;
            }
            _ => {}
        }
    }
    store.record_action_evidence(work, evidence, false).await?;
    Ok(())
}

fn retry_prompt(work: &ActionWork, evidence: &PublicationEvidence) -> Option<MessageBody> {
    let mut prompt = format!(
        "Continue only the publication explicitly authorized by the original ReviewQ request.          Action {}, requested in native turn {}, user entry {}, reviewed SHA {}.          Read the exact original instruction. Independently verify the source head and authenticated          account before acting. If either changed or the exact original body cannot be recovered, stop.          Reconcile after the attempt. Do not repeat confirmed targets, edit existing notes,          resolve threads, merge, change Jira, or perform any additional operation.",
        work.action.id,
        work.action.turn.as_ref()?,
        work.action.request_entry.as_ref()?,
        evidence.auth_head,
    );
    for (target, observed) in work.action.targets.iter().zip(&evidence.targets) {
        if observed.state == Evidence::Absent {
            prompt.push_str(&format!(
                "\nPublish the exact original UTF-8 body with SHA-256 {} to {}.",
                target.body_sha256,
                target
                    .discussion_id
                    .as_deref()
                    .unwrap_or("a top-level MR comment")
            ));
        }
    }
    if evidence.approval == Approval::Absent {
        prompt.push_str(
            "\nComplete the explicitly requested approval for this authenticated account and SHA.",
        );
    }
    MessageBody::new(&prompt).ok()
}
