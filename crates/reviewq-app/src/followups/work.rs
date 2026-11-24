use std::collections::BTreeSet;

use reviewq_domain::identity::{GitSha, NativeTurnId, Revision};
use reviewq_domain::session::LeadTarget;

use crate::access::{AppError, AppFuture};
use crate::hosts::MessageBody;
use crate::queue::MergeRequestState;
use crate::reports::ReportReference;
use crate::sources::ReviewSourceReader;
use crate::workflow::{NativeWorkflowProof, ReadyWorkflow, SelectedNote, WorkflowReadiness};

use super::MonitoredReview;

pub const MAX_FROZEN_OBSERVATIONS: u16 = 10;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckMember {
    pub observation_id: String,
    pub discussion_id: String,
    pub discussion_version: String,
    pub originals: Vec<SelectedNote>,
    pub finding_ids: Vec<String>,
    pub prior_batches: Vec<String>,
}

pub struct PendingCheck {
    pub batch_id: String,
    pub monitor: MonitoredReview,
    pub lead: LeadTarget,
    pub authenticated_reviewer: u64,
    pub configured_reviewers: BTreeSet<u64>,
    pub members: Vec<CheckMember>,
    pub base: Option<ReportReference>,
    pub disposition_version: Revision,
}

pub struct PreparedCheck {
    pub pending: PendingCheck,
    pub checked_head: GitSha,
    pub ready: ReadyWorkflow,
    pub prompt: MessageBody,
}

pub struct ExecutingCheck {
    pub batch_id: String,
    pub lead: LeadTarget,
    pub turn: NativeTurnId,
}

pub trait FollowupWorkerStore: Send + Sync {
    fn checks_awaiting_idle(&self) -> AppFuture<'_, Vec<ExecutingCheck>>;
    fn observe_check_idle(&self, check: ExecutingCheck) -> AppFuture<'_, ()>;
    fn due_checks(&self, limit: u16) -> AppFuture<'_, Vec<PendingCheck>>;
    fn hold_check(&self, check: PendingCheck, reason: &'static str) -> AppFuture<'_, ()>;
    fn reserve_check(&self, check: PreparedCheck) -> AppFuture<'_, ()>;
}

pub async fn run_due_followups(
    store: &dyn FollowupWorkerStore,
    source: &dyn ReviewSourceReader,
    native: &dyn NativeWorkflowProof,
) -> Result<(), AppError> {
    for check in store.checks_awaiting_idle().await? {
        if native
            .ended_and_idle(check.lead.clone(), check.turn.clone())
            .await
            == Ok(true)
            && let Err(error) = store.observe_check_idle(check).await
            && !matches!(error, AppError::StaleRevision | AppError::Conflict)
        {
            return Err(error);
        }
    }
    for pending in store.due_checks(4).await? {
        match prepare(&pending, source, native).await {
            Ok((checked_head, ready, prompt)) => {
                let result = store
                    .reserve_check(PreparedCheck {
                        pending,
                        checked_head,
                        ready,
                        prompt,
                    })
                    .await;
                if let Err(error) = result
                    && !matches!(
                        error,
                        AppError::StaleRevision | AppError::Conflict | AppError::Busy
                    )
                {
                    return Err(error);
                }
            }
            Err(reason) => {
                let result = store.hold_check(pending, reason).await;
                if let Err(error) = result
                    && !matches!(error, AppError::StaleRevision | AppError::Conflict)
                {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

async fn prepare(
    check: &PendingCheck,
    source: &dyn ReviewSourceReader,
    native: &dyn NativeWorkflowProof,
) -> Result<(GitSha, ReadyWorkflow, MessageBody), &'static str> {
    let source = source
        .workflow_snapshot(check.monitor.identity.clone())
        .await
        .map_err(|_| "followup_source_unavailable")?;
    if source.review.identity != check.monitor.identity
        || source.review.state != MergeRequestState::Open
    {
        return Err("followup_merge_request_unavailable");
    }
    if source.authenticated_actor != check.authenticated_reviewer {
        return Err("monitored_reviewer_changed");
    }
    match source.reviewer_approval(&check.configured_reviewers) {
        Some(false) => {}
        Some(true) => return Err("reviewer_approval_confirmed"),
        None => return Err("reviewer_approval_unknown"),
    }
    for member in &check.members {
        if !source.discussions.iter().any(|discussion| {
            discussion.id == member.discussion_id
                && member.originals.iter().all(|original| {
                    check.configured_reviewers.contains(&original.reviewer_id)
                        && discussion.notes.iter().any(|note| {
                            note.author == original.reviewer_id && note.id == original.note_id
                        })
                })
        }) {
            return Err("followup_selected_discussion_unavailable");
        }
    }
    let ready = match native.followup_readiness(check.lead.clone()).await {
        Ok(WorkflowReadiness::Ready(ready)) => ready,
        Ok(WorkflowReadiness::Held(reason)) => return Err(reason.reason()),
        Err(_) => return Err("native_provider_unavailable"),
    };
    if ready.model.is_empty()
        || ready.model.len() > 128
        || ready.effort.is_empty()
        || ready.effort.len() > 64
    {
        return Err("native_settings_unproved");
    }
    let mut prompt = format!(
        concat!(
            "Privately reassess selected ReviewQ discussions for {} !{} at source SHA {}. Batch {}. ",
            "Read each identified discussion and the current code. ",
            "Preserve unrelated findings, questions, qualifications and user dispositions. ",
            "Do not post or edit GitLab notes, approve, merge, resolve discussions, or change Jira. ",
            "Submit report_handoff with one evidence-backed assessment for every listed discussion, ",
            "covering its finding IDs, and an outcome in findingChanges for each finding you re-assess. ",
            "Findings you do not re-assess are carried unchanged."
        ),
        source.review.project_path,
        source.review.identity.iid(),
        source.review.head_sha,
        check.batch_id,
    );
    if let Some(base) = &check.base {
        prompt.push_str(&format!(
            "\nAccepted baseline receipt {} (SHA-256 {}).",
            base.receipt_id, base.hash
        ));
    }
    for member in &check.members {
        prompt.push_str(&format!(
            "\nDiscussion {} original note {} observation {} at {}. Finding IDs: {}.",
            member.discussion_id,
            member
                .originals
                .iter()
                .map(|original| original.note_id.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            member.observation_id,
            member.discussion_version,
            member.finding_ids.join(", ")
        ));
        if !member.prior_batches.is_empty() {
            prompt.push_str(&format!(
                " Reconciles earlier batches {}.",
                member.prior_batches.join(", ")
            ));
        }
    }
    let prompt = MessageBody::new(&prompt).map_err(|_| "followup_prompt_too_large")?;
    Ok((source.review.head_sha, ready, prompt))
}
