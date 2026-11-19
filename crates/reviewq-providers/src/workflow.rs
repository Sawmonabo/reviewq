//! Selects the proof reader by the registered native binding.

use std::sync::Arc;

use reviewq_app::access::{AppError, AppFuture};
use reviewq_app::workflow::{
    ExpectedAcknowledgement, NativeCall, NativeWorkflowProof, ReportCompletion, WorkflowTool,
};
use reviewq_domain::identity::NativeTurnId;
use reviewq_domain::session::{HostProvider, LeadTarget};

use crate::Codex;

pub struct Workflows {
    codex: Option<Arc<Codex>>,
}

impl Workflows {
    pub fn new(codex: Option<Arc<Codex>>) -> Self {
        Self { codex }
    }

    fn reader(&self, lead: &LeadTarget) -> Result<&dyn NativeWorkflowProof, AppError> {
        match lead.binding().identity().provider() {
            HostProvider::Codex => self
                .codex
                .as_deref()
                .map(|codex| codex as &dyn NativeWorkflowProof),
            HostProvider::Claude => None,
        }
        .ok_or(AppError::Unavailable)
    }
}

impl NativeWorkflowProof for Workflows {
    fn verify_active_call(
        &self,
        lead: LeadTarget,
        turn: Option<NativeTurnId>,
        call_id: String,
        tool: WorkflowTool,
    ) -> AppFuture<'_, NativeCall> {
        Box::pin(async move {
            self.reader(&lead)?
                .verify_active_call(lead, turn, call_id, tool)
                .await
        })
    }

    fn report_completion(
        &self,
        call: NativeCall,
        acknowledgement: ExpectedAcknowledgement,
    ) -> AppFuture<'_, ReportCompletion> {
        Box::pin(async move {
            self.reader(&call.lead)?
                .report_completion(call, acknowledgement)
                .await
        })
    }
}
