//! Provider-established native call identity and completion evidence.

use std::sync::Arc;

use reviewq_domain::identity::{
    ActionId, GitSha, MrIdentity, NativeInputId, NativeTurnId, ReceiptId, ReportHash, Revision,
    SubmissionId,
};
use reviewq_domain::session::LeadTarget;

use crate::access::AppFuture;
use crate::hosts::MessageBody;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowTool {
    Report,
    Action,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCall {
    pub lead: LeadTarget,
    pub turn_id: NativeTurnId,
    pub root_turn_id: Option<NativeTurnId>,
    pub call_id: String,
    pub source_generation: String,
    pub source_position: u64,
    pub request_entry_id: Option<NativeInputId>,
    pub tool: WorkflowTool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedAcknowledgement {
    pub receipt_id: ReceiptId,
    pub declaration_sha256: ReportHash,
}

pub enum ReportCompletion {
    Pending,
    Unproved,
    Confirmed(CompletedReportCall),
}

pub struct CompletedReportCall {
    pub call: NativeCall,
    pub acknowledged: ExpectedAcknowledgement,
    pub result_entry_id: NativeInputId,
    pub normal_turn_completion: bool,
    pub final_response: Option<Arc<dyn ReportContent>>,
}

/// One confirmed private native source. A reader keeps the source generation fixed.
pub trait ReportContent: Send + Sync {
    fn byte_length(&self) -> u64;
    fn read(&self, offset: u64, length: usize) -> AppFuture<'_, Vec<u8>>;
    fn verify_unchanged(&self) -> AppFuture<'_, ()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadyWorkflow {
    pub model: String,
    pub effort: String,
    pub latest_user_entry_id: Option<NativeInputId>,
    pub latest_user_turn_id: Option<NativeTurnId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowHold {
    ProviderUnavailable,
    ActiveWork,
    PendingNativeInput,
    UnknownSettings,
    MissingCompanion,
    IdleSendUnavailable,
}

impl WorkflowHold {
    pub fn reason(self) -> &'static str {
        match self {
            Self::ProviderUnavailable => "native_provider_unavailable",
            Self::ActiveWork => "native_work_active",
            Self::PendingNativeInput => "native_input_required",
            Self::UnknownSettings => "native_settings_unproved",
            Self::MissingCompanion => "native_companion_unavailable",
            Self::IdleSendUnavailable => "native_idle_only_delivery_unavailable",
        }
    }
}

pub enum WorkflowReadiness {
    Ready(ReadyWorkflow),
    Held(WorkflowHold),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkflowBinding {
    Action(ActionId),
    Followup(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowGuard {
    pub binding: WorkflowBinding,
    pub authority_revision: Revision,
    pub settings: Revision,
    pub head: GitSha,
    pub ready: ReadyWorkflow,
}

pub struct WorkflowAdmission {
    pub target: LeadTarget,
    pub submission: SubmissionId,
    pub body: MessageBody,
    pub binding: WorkflowBinding,
    pub settings: Revision,
    pub head: GitSha,
    pub ready: ReadyWorkflow,
}

pub struct WorkflowRequirements {
    pub identity: MrIdentity,
    pub reviewer_id: u64,
    pub requirement: WorkflowRequirement,
}

pub enum WorkflowRequirement {
    Publication {
        remaining_targets: Vec<crate::actions::CommentTarget>,
        approval_pending: bool,
    },
    Followup {
        discussions: Vec<SelectedDiscussion>,
        configured_reviewers: std::collections::BTreeSet<u64>,
    },
}

pub struct SelectedDiscussion {
    pub discussion_id: String,
    pub originals: Vec<SelectedNote>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SelectedNote {
    pub note_id: u64,
    pub reviewer_id: u64,
}

impl WorkflowRequirements {
    /// Verifies fresh provider evidence immediately before a workflow input may be sent.
    pub fn check(
        &self,
        source: &crate::sources::WorkflowSnapshot,
        pinned_head: &GitSha,
    ) -> Result<(), crate::access::AppError> {
        use crate::access::AppError;
        if source.review.identity != self.identity
            || &source.review.head_sha != pinned_head
            || source.review.state != crate::queue::MergeRequestState::Open
            || source.authenticated_actor != self.reviewer_id
        {
            return Err(AppError::StaleRevision);
        }
        match &self.requirement {
            WorkflowRequirement::Publication {
                remaining_targets,
                approval_pending,
            } => {
                if (*approval_pending && source.personal_approval != Some(false))
                    || remaining_targets.iter().any(|target| {
                        !crate::actions::proves_target_absence(source, target, pinned_head)
                    })
                {
                    return Err(AppError::Conflict);
                }
            }
            WorkflowRequirement::Followup {
                discussions,
                configured_reviewers,
            } => {
                if source.reviewer_approval(configured_reviewers) != Some(false)
                    || discussions.iter().any(|expected| {
                        !source.discussions.iter().any(|actual| {
                            actual.id == expected.discussion_id
                                && expected.originals.iter().all(|original| {
                                    configured_reviewers.contains(&original.reviewer_id)
                                        && actual.notes.iter().any(|note| {
                                            note.id == original.note_id
                                                && note.author == original.reviewer_id
                                        })
                                })
                        })
                    })
                {
                    return Err(AppError::Conflict);
                }
            }
        }
        Ok(())
    }
}

pub trait NativeWorkflowProof: Send + Sync {
    fn observe_attention(
        &self,
        _target: crate::notifications::TrackedNative,
    ) -> AppFuture<'_, Option<crate::notifications::NativeAttention>> {
        Box::pin(async { Ok(None) })
    }

    /// Proves current idle eligibility and exact effective settings without starting or resuming a turn.
    fn dispatch_readiness(&self, _lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        Box::pin(async { Ok(WorkflowReadiness::Held(WorkflowHold::ProviderUnavailable)) })
    }

    /// Resolve and verify an actual active lead tool from the selected native source.
    /// Reads current idle state and proves this exact turn has ended.
    fn ended_and_idle(&self, _lead: LeadTarget, _turn: NativeTurnId) -> AppFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }

    fn verify_active_call(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall>;

    /// Verify the matching structured acknowledgement from this exact completed call.
    fn report_completion(
        &self,
        call: NativeCall,
        acknowledgement: ExpectedAcknowledgement,
    ) -> AppFuture<'_, ReportCompletion>;
}
