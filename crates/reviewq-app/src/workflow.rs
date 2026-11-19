//! Provider-established native call identity and completion evidence.

use std::sync::Arc;

use reviewq_domain::identity::{NativeInputId, NativeTurnId, ReceiptId, ReportHash};
use reviewq_domain::session::LeadTarget;

use crate::access::AppFuture;

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

pub trait NativeWorkflowProof: Send + Sync {
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
