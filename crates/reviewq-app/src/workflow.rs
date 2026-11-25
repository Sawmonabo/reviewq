//! Provider-established native call identity and workflow readiness.

use reviewq_domain::identity::{
    ActionId, GitSha, MrIdentity, NativeInputId, NativeTurnId, Revision, SubmissionId,
};
use reviewq_domain::session::LeadTarget;

use crate::access::{AppError, AppFuture};
use crate::hosts::{MessageBody, MessagePurpose};
use crate::queue::MergeRequestState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowTool {
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
pub struct ReadyWorkflow {
    pub model: String,
    pub effort: String,
    pub latest_user_entry_id: Option<NativeInputId>,
    pub latest_user_turn_id: Option<NativeTurnId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowHold {
    ProviderUnavailable,
    SessionNotLoaded,
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
            Self::SessionNotLoaded => "native_session_not_loaded",
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
    Recovery(crate::hosts::recovery::RecoveryAuthorization),
}

impl WorkflowBinding {
    pub fn message_purpose(&self) -> MessagePurpose {
        match self {
            Self::Action(_) => MessagePurpose::Publication,
            Self::Followup(_) => MessagePurpose::ThreadFollowup,
            Self::Recovery(_) => MessagePurpose::CodexRecovery,
        }
    }
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
    Recovery,
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
    pub(crate) fn check_review(
        &self,
        identity: &MrIdentity,
        head: &GitSha,
        state: MergeRequestState,
        actor: u64,
        pinned_head: &GitSha,
    ) -> Result<(), AppError> {
        if identity != &self.identity
            || head != pinned_head
            || state != MergeRequestState::Open
            || actor != self.reviewer_id
        {
            return Err(AppError::StaleRevision);
        }
        Ok(())
    }

    /// Verifies fresh provider evidence immediately before a workflow input may be sent.
    pub fn check(
        &self,
        source: &crate::sources::WorkflowSnapshot,
        pinned_head: &GitSha,
    ) -> Result<(), AppError> {
        self.check_review(
            &source.review.identity,
            &source.review.head_sha,
            source.review.state,
            source.authenticated_actor,
            pinned_head,
        )?;
        match &self.requirement {
            WorkflowRequirement::Recovery => {}
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
    fn prepare_recovery(
        &self,
        _candidate: &crate::hosts::recovery::Candidate,
    ) -> AppFuture<'_, (crate::hosts::recovery::RecoveryAuthorization, ReadyWorkflow)> {
        Box::pin(async { Err(crate::access::AppError::Unavailable) })
    }

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

    fn followup_readiness(&self, lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        self.dispatch_readiness(lead)
    }

    /// Reads current idle state and proves this exact turn has ended.
    fn ended_and_idle(&self, _lead: LeadTarget, _turn: NativeTurnId) -> AppFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }

    /// Resolve and verify an actual active lead tool from the selected native source.
    fn verify_active_call(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall>;
}
