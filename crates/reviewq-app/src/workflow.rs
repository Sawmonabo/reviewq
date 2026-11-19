//! Provider-established native call identity and completion evidence.

use std::sync::Arc;

use reviewq_domain::identity::{
    ActionId, GitSha, NativeInputId, NativeTurnId, ReceiptId, ReportHash, Revision, SubmissionId,
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
}

impl WorkflowHold {
    pub fn reason(self) -> &'static str {
        match self {
            Self::ProviderUnavailable => "native_provider_unavailable",
            Self::ActiveWork => "native_work_active",
            Self::PendingNativeInput => "native_input_required",
            Self::UnknownSettings => "native_settings_unproved",
            Self::MissingCompanion => "native_companion_unavailable",
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

pub trait NativeWorkflowProof: Send + Sync {
    /// Proves current idle eligibility and exact effective settings without starting or resuming a turn.
    fn dispatch_readiness(&self, _lead: LeadTarget) -> AppFuture<'_, WorkflowReadiness> {
        Box::pin(async { Ok(WorkflowReadiness::Held(WorkflowHold::ProviderUnavailable)) })
    }

    /// Resolve and verify an actual active lead tool from the selected native source.
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
